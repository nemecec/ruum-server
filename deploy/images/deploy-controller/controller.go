package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"regexp"
	"sort"
	"strings"
	"sync"
	"time"
)

// The annotations and keys that the controller writes on a request. The deployer may only
// create requests (no update, no delete), so it cannot change them after the controller did.
const (
	requestLabel = "ruum.space/release-request"
	annPhase     = "ruum.space/phase"
	annClaimedBy = "ruum.space/claimed-by"
	annStarted   = "ruum.space/started"
	annFinished  = "ruum.space/finished"
	keyStatus    = "status"
	keyLog       = "log"
)

// The phases of a request.
const (
	PhaseRunning   = "Running"
	PhaseSucceeded = "Succeeded"
	PhaseFailed    = "Failed"
	PhaseRefused   = "Refused"
)

// Deployer runs an accepted plan (ops/deploy-chart.sh).
type Deployer interface {
	Run(ctx context.Context, args []string, line func(string)) error
}

// Controller takes release requests one at a time, oldest first.
type Controller struct {
	Store    Store
	Checker  Checker
	Deployer Deployer
	// Me names this pod in the claim annotation.
	Me string
	// MaxAge: a request that waited longer than this before the controller saw it is refused, so
	// an old request never deploys by surprise after a controller outage. deploy.sh waits less.
	MaxAge time.Duration
	// Keep: finished requests older than this are deleted.
	Keep time.Duration
	// Timeout of one deploy (deploy-chart.sh bounds its steps below it).
	Timeout time.Duration
	// LogLines: the last lines of the deploy output kept in the request.
	LogLines int
	// ProgressEvery: how often the output so far is written into the request.
	ProgressEvery time.Duration
	Now           func() time.Time
	Log           io.Writer
}

func (c *Controller) logf(format string, args ...any) {
	fmt.Fprintf(c.Log, "%s deploy-controller: %s\n", c.Now().UTC().Format(time.RFC3339), fmt.Sprintf(format, args...))
}

// RecoverInterrupted marks the requests that a previous controller pod left Running as Failed:
// its deploy-chart.sh died with it. The deploy lock (Lease ruum/ruum-deploy) stays until it
// expires, so a new request waits for it or is refused by deploy-chart.sh.
func (c *Controller) RecoverInterrupted(ctx context.Context) error {
	items, err := c.Store.List(ctx)
	if err != nil {
		return err
	}
	for _, cm := range items {
		if cm.Metadata.Annotations[annPhase] != PhaseRunning || cm.Metadata.Annotations[annClaimedBy] == c.Me {
			continue
		}
		c.logf("request %s was Running in %s, which is gone", cm.Metadata.Name, cm.Metadata.Annotations[annClaimedBy])
		c.finish(ctx, cm, PhaseFailed, "the deploy controller restarted during this deploy; check the release (status.sh) and run deploy.sh again (the deploy lock expires after at most 30 minutes)", nil)
	}
	return nil
}

// Reconcile takes the oldest new request, if any, and runs it to the end. It returns after one
// request, so the caller can call it in a loop.
func (c *Controller) Reconcile(ctx context.Context) error {
	items, err := c.Store.List(ctx)
	if err != nil {
		return err
	}
	c.cleanup(ctx, items)
	var fresh []ConfigMap
	for _, cm := range items {
		if cm.Metadata.Annotations[annPhase] == "" {
			fresh = append(fresh, cm)
		}
	}
	sort.Slice(fresh, func(i, j int) bool {
		a, b := fresh[i].Metadata, fresh[j].Metadata
		if !a.CreationTimestamp.Equal(b.CreationTimestamp) {
			return a.CreationTimestamp.Before(b.CreationTimestamp)
		}
		return a.Name < b.Name
	})
	for _, cm := range fresh {
		if c.Now().Sub(cm.Metadata.CreationTimestamp) > c.MaxAge {
			c.logf("request %s is older than %s; refused", cm.Metadata.Name, c.MaxAge)
			c.finish(ctx, cm, PhaseRefused, fmt.Sprintf("the request waited more than %s before the deploy controller saw it; run deploy.sh again", c.MaxAge), nil)
			continue
		}
		claimed, err := c.claim(ctx, cm)
		if errors.Is(err, ErrConflict) || errors.Is(err, ErrNotFound) {
			continue
		}
		if err != nil {
			// For example an immutable ConfigMap: skip it, so one bad request never blocks the
			// others; it is never run, because it cannot be claimed.
			c.logf("request %s cannot be claimed: %v", cm.Metadata.Name, err)
			continue
		}
		c.run(ctx, claimed)
		return nil
	}
	return nil
}

func (c *Controller) claim(ctx context.Context, cm ConfigMap) (ConfigMap, error) {
	if cm.Metadata.Annotations == nil {
		cm.Metadata.Annotations = map[string]string{}
	}
	cm.Metadata.Annotations[annPhase] = PhaseRunning
	cm.Metadata.Annotations[annClaimedBy] = c.Me
	cm.Metadata.Annotations[annStarted] = c.Now().UTC().Format(time.RFC3339)
	return c.Store.Update(ctx, cm)
}

// run checks and deploys one claimed request and writes the outcome back.
func (c *Controller) run(ctx context.Context, cm ConfigMap) {
	name := cm.Metadata.Name
	data := map[string]string{}
	for k, v := range cm.Data {
		data[k] = v
	}
	req, err := ParseRequest(data)
	if err != nil {
		c.logf("request %s refused: %v", name, err)
		c.finishByName(ctx, name, PhaseRefused, err.Error(), nil)
		return
	}
	c.logf("request %s: commit %s, server %s, edge %s, live %s, rollback %s", name, req.Commit, req.Server, req.Edge, req.ExpectDeployed, req.Rollback)
	checkCtx, cancel := context.WithTimeout(ctx, 5*time.Minute)
	plan, err := c.Checker.Check(checkCtx, req)
	cancel()
	if err != nil {
		phase := PhaseFailed
		if IsRefusal(err) {
			phase = PhaseRefused
		}
		c.logf("request %s %s: %v", name, strings.ToLower(phase), err)
		c.finishByName(ctx, name, phase, err.Error(), nil)
		return
	}
	out := &tail{max: c.LogLines}
	for _, n := range plan.Notes {
		out.add(n)
	}
	out.add("checks passed: tag, provenance (cosign) and revision of both images, on main, release floor, order against the live release")
	c.progress(ctx, name, out, "checks passed; deploying (deploy lock, backup if needed, migration step, rolling update)")
	runCtx, cancel := context.WithTimeout(ctx, c.Timeout)
	defer cancel()
	// The output so far goes into the request every ProgressEvery while it grows, so the
	// deployer sees where a long step (backup, migration Job, rollout) is.
	stop := make(chan struct{})
	ticker := make(chan struct{})
	written := out.count() // the progress call above wrote these
	go func() {
		defer close(ticker)
		for {
			select {
			case <-stop:
				return
			case <-time.After(c.ProgressEvery):
				if n := out.count(); n != written {
					written = n
					c.progress(ctx, name, out, "deploying")
				}
			}
		}
	}()
	err = c.Deployer.Run(runCtx, plan.Args, func(line string) {
		// The full output, the migration Job's log included, goes to this pod's log (admin
		// only). The request, which the deployer reads, gets it without the Job's log lines and
		// with any credentials in URLs masked.
		fmt.Fprintf(c.Log, "%s | %s\n", name, line)
		if strings.HasPrefix(line, "migrate| ") {
			return
		}
		out.add(redact(line))
	})
	close(stop)
	<-ticker
	if err != nil {
		c.logf("request %s failed: %v", name, err)
		c.finishByName(ctx, name, PhaseFailed, fmt.Sprintf("the deploy failed (%v); the migration Job log is in the controller's log: kubectl -n ruum-deploy logs deploy/deploy-controller (admin)", err), out)
		return
	}
	c.logf("request %s succeeded", name)
	c.finishByName(ctx, name, PhaseSucceeded, "deployed "+req.Commit, out)
}

// progress writes the status and the log so far (best effort).
func (c *Controller) progress(ctx context.Context, name string, out *tail, status string) {
	for range 3 {
		cm, err := c.Store.Get(ctx, name)
		if err != nil {
			return
		}
		if cm.Data == nil {
			cm.Data = map[string]string{}
		}
		cm.Data[keyStatus] = status
		cm.Data[keyLog] = out.String()
		if _, err := c.Store.Update(ctx, cm); !errors.Is(err, ErrConflict) {
			return
		}
	}
}

func (c *Controller) finishByName(ctx context.Context, name, phase, status string, out *tail) {
	for range 5 {
		cm, err := c.Store.Get(ctx, name)
		if err != nil {
			c.logf("request %s: cannot write the outcome %s: %v", name, phase, err)
			return
		}
		if c.finish(ctx, cm, phase, status, out) {
			return
		}
	}
}

// finish writes the outcome; false on a conflict (the caller reads again).
func (c *Controller) finish(ctx context.Context, cm ConfigMap, phase, status string, out *tail) bool {
	if cm.Metadata.Annotations == nil {
		cm.Metadata.Annotations = map[string]string{}
	}
	if cm.Data == nil {
		cm.Data = map[string]string{}
	}
	cm.Metadata.Annotations[annPhase] = phase
	cm.Metadata.Annotations[annFinished] = c.Now().UTC().Format(time.RFC3339)
	if cm.Metadata.Annotations[annClaimedBy] == "" {
		cm.Metadata.Annotations[annClaimedBy] = c.Me
	}
	cm.Data[keyStatus] = redact(status)
	if out != nil {
		cm.Data[keyLog] = out.String()
	}
	_, err := c.Store.Update(ctx, cm)
	if err != nil && !errors.Is(err, ErrConflict) {
		c.logf("request %s: cannot write the outcome %s: %v", cm.Metadata.Name, phase, err)
		return true
	}
	return err == nil
}

// cleanup deletes finished requests older than Keep.
func (c *Controller) cleanup(ctx context.Context, items []ConfigMap) {
	for _, cm := range items {
		switch cm.Metadata.Annotations[annPhase] {
		case PhaseSucceeded, PhaseFailed, PhaseRefused:
		default:
			continue
		}
		if c.Now().Sub(cm.Metadata.CreationTimestamp) > c.Keep {
			if err := c.Store.Delete(ctx, cm.Metadata.Name); err == nil {
				c.logf("deleted the old request %s", cm.Metadata.Name)
			}
		}
	}
}

// userinfoRE matches the user and password part of a URL (postgres://user:password@host).
var userinfoRE = regexp.MustCompile(`([A-Za-z][A-Za-z0-9+.-]*://)[^\s/@]+@`)

// redact masks credentials in URLs.
func redact(s string) string { return userinfoRE.ReplaceAllString(s, "${1}[redacted]@") }

// tail keeps the last max lines.
type tail struct {
	mu    sync.Mutex
	max   int
	lines []string
	added int
}

// count is the number of lines added so far (also the ones that fell out).
func (t *tail) count() int {
	t.mu.Lock()
	defer t.mu.Unlock()
	return t.added
}

func (t *tail) add(line string) {
	t.mu.Lock()
	defer t.mu.Unlock()
	if len(line) > 2000 {
		line = line[:2000] + " [cut]"
	}
	t.lines = append(t.lines, line)
	t.added++
	if t.max > 0 && len(t.lines) > t.max {
		t.lines = t.lines[len(t.lines)-t.max:]
	}
}

func (t *tail) String() string {
	t.mu.Lock()
	defer t.mu.Unlock()
	return strings.Join(t.lines, "\n")
}

package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"regexp"
	"slices"
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
	// Keep: requests older than this are deleted, whatever their phase (a request that the
	// deployer created with a preset phase, or that cannot be claimed, is never run, so it would
	// stay forever otherwise).
	Keep time.Duration
	// KeepCount: at most this many finished requests are kept (the newest); older ones are
	// deleted before Keep. With the ResourceQuota of ruum-deploy this bounds what the requests
	// hold in etcd.
	KeepCount int
	// Commit is the commit of main this controller's image was built from: the chart, the
	// migration Job and deploy-chart.sh of that commit run (charts/platform sets it).
	Commit string
	// Timeout of one deploy (deploy-chart.sh bounds its steps below it).
	Timeout time.Duration
	// LogLines: the last lines of the deploy output kept in the request.
	LogLines int
	// ProgressEvery: how often the output so far is written into the request.
	ProgressEvery time.Duration
	Now           func() time.Time
	Log           io.Writer

	// current is the request that run works on now (cleanup leaves it alone).
	mu      sync.Mutex
	current string
}

func (c *Controller) setCurrent(name string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.current = name
}

func (c *Controller) isCurrent(name string) bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	return name != "" && c.current == name
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
		// After SIGTERM no new request is taken (a running one finishes, see run).
		if ctx.Err() != nil {
			return nil
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

// run checks and deploys one claimed request and writes the outcome back. A stop of the pod
// (SIGTERM) does not cancel it: deploy-chart.sh killed in the middle of helm upgrade leaves the
// release pending-upgrade and the deploy lock held. The pod's terminationGracePeriodSeconds is
// longer than Timeout, so the deploy finishes and its outcome is written before the pod goes.
func (c *Controller) run(ctx context.Context, cm ConfigMap) {
	ctx = context.WithoutCancel(ctx)
	name := cm.Metadata.Name
	c.setCurrent(name)
	defer c.setCurrent("")
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
	commit := c.Commit
	if commit == "" {
		commit = "unknown"
	}
	out.add(fmt.Sprintf("deploy controller of commit %s: the chart, the migration Job and deploy-chart.sh of that commit run (a newer chart reaches staging with the platform step)", commit))
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
	// The migration Job's log, kept apart: it reaches the request only when the deploy fails
	// (backlog O-50: the deployer reads no pod log, so the request must say why), as its last
	// MigrateLines lines with secret-looking values masked (redactSecrets).
	migrate := &tail{max: MigrateLines}
	keep := []string{req.Commit, req.ExpectDeployed, c.Commit}
	err = c.Deployer.Run(runCtx, plan.Args, func(line string) {
		// The full output, the migration Job's log included, goes to this pod's log (admin
		// only). The request, which the deployer reads, gets every line through the full
		// secret mask (helm's error and the warning events too, not only the Job's log).
		fmt.Fprintf(c.Log, "%s | %s\n", name, line)
		if strings.HasPrefix(line, "migrate| ") {
			migrate.add(redactSecrets(line, keep...))
			return
		}
		out.add(redactSecrets(line, keep...))
	})
	close(stop)
	<-ticker
	if err != nil {
		c.logf("request %s failed: %v", name, err)
		if n := migrate.count(); n > 0 {
			out.add(fmt.Sprintf("the last %d lines of the migration Job's log (secret-looking values masked):", min(n, MigrateLines)))
			for _, l := range migrate.all() {
				out.add(l)
			}
		}
		c.finishByName(ctx, name, PhaseFailed, fmt.Sprintf("the deploy failed (%v); the reason is in the log of this request (the migration Job's log tail, the helm error, the warning events)", err), out)
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
	cm.Data[keyStatus] = redactSecrets(status, cm.Data["commit"], cm.Data["expectDeployed"], c.Commit)
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

// cleanup deletes every request older than Keep, whatever its phase, and the finished requests
// beyond the newest KeepCount. Only the request that run works on now stays (Reconcile is
// serial, so at this point there is none; the check guards a later change).
func (c *Controller) cleanup(ctx context.Context, items []ConfigMap) {
	var finished []ConfigMap
	for _, cm := range items {
		if c.isCurrent(cm.Metadata.Name) {
			continue
		}
		if c.Now().Sub(cm.Metadata.CreationTimestamp) > c.Keep {
			c.remove(ctx, cm.Metadata.Name, "older than "+c.Keep.String())
			continue
		}
		switch cm.Metadata.Annotations[annPhase] {
		case PhaseSucceeded, PhaseFailed, PhaseRefused:
			finished = append(finished, cm)
		}
	}
	if c.KeepCount <= 0 || len(finished) <= c.KeepCount {
		return
	}
	sort.Slice(finished, func(i, j int) bool {
		a, b := finished[i].Metadata, finished[j].Metadata
		if !a.CreationTimestamp.Equal(b.CreationTimestamp) {
			return a.CreationTimestamp.After(b.CreationTimestamp)
		}
		return a.Name > b.Name
	})
	for _, cm := range finished[c.KeepCount:] {
		c.remove(ctx, cm.Metadata.Name, fmt.Sprintf("more than %d finished requests", c.KeepCount))
	}
}

func (c *Controller) remove(ctx context.Context, name, why string) {
	if err := c.Store.Delete(ctx, name); err == nil {
		c.logf("deleted the request %s (%s)", name, why)
	} else if !errors.Is(err, ErrNotFound) {
		c.logf("cannot delete the request %s: %v", name, err)
	}
}

// userinfoRE matches the user and password part of a URL (postgres://user:password@host).
var userinfoRE = regexp.MustCompile(`([A-Za-z][A-Za-z0-9+.-]*://)[^\s/@]+@`)

// redact masks credentials in URLs.
func redact(s string) string { return userinfoRE.ReplaceAllString(s, "${1}[redacted]@") }

// MigrateLines is how many lines of the migration Job's log a failed request shows.
const MigrateLines = 100

// keyRE matches a key and its separator: password=, "token": , api-key: , \"secret\":.
// The caller checks that the key starts a word (Go regexp has no look-behind).
var keyRE = regexp.MustCompile(`(\\?["']?)([A-Za-z][A-Za-z0-9_.-]*)(\\?["']?)\s*[:=]\s*`)

// secretWords are the words of a key whose value is masked. A key is split into words at
// "_", "-", "." and camelCase borders, so API_KEY, apiKey, private-key and key match, monkey not.
var secretWords = map[string]bool{
	"key": true, "keys": true, "apikey": true, "password": true, "passwd": true, "pwd": true,
	"passphrase": true, "pass": true, "secret": true, "secrets": true, "token": true, "tokens": true,
	"credential": true, "credentials": true, "authorization": true, "auth": true, "cookie": true,
	"bearer": true, "signature": true, "dsn": true,
}

// authSchemes are the words that come before a credential in an Authorization value.
var authSchemes = map[string]bool{"bearer": true, "basic": true, "token": true, "digest": true, "negotiate": true}

// bearerRE matches a bearer credential without a key (Bearer abc).
var bearerRE = regexp.MustCompile(`(?i)\b(bearer\s+)[^\s"',;]+`)

// jwtRE matches a JSON Web Token (three base64url parts with dots).
var jwtRE = regexp.MustCompile(`eyJ[A-Za-z0-9_-]*\.[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]*)?`)

// longTokenRE matches long runs that look like keys or tokens (40 or more base64 or hex characters).
var longTokenRE = regexp.MustCompile(`[A-Za-z0-9+/_=-]{40,}`)

// hex64RE is an image digest after "sha256:" (public, not a secret).
var hex64RE = regexp.MustCompile(`^[0-9a-f]{64}$`)

const masked = "[redacted]"

// redactSecrets masks credentials in URLs, the values of secret-looking keys (the full value:
// quoted with escapes and spaces, or unquoted to the next space; the rest of the line when the
// value cannot be parsed for sure), bearer credentials, JWTs and long tokens. keep are values
// that may stay (the commits of the request: public, and the deployer needs to see them).
// Every line that goes into a request passes this function.
func redactSecrets(s string, keep ...string) string {
	s = redact(s)
	s = maskKeyValues(s)
	s = bearerRE.ReplaceAllString(s, "${1}"+masked)
	s = jwtRE.ReplaceAllString(s, masked)
	var b strings.Builder
	last := 0
	for _, m := range longTokenRE.FindAllStringIndex(s, -1) {
		tok := s[m[0]:m[1]]
		if keepToken(tok, s[:m[0]], keep) {
			continue
		}
		b.WriteString(s[last:m[0]])
		b.WriteString(masked)
		last = m[1]
	}
	b.WriteString(s[last:])
	return b.String()
}

func keepToken(tok, before string, keep []string) bool {
	if hex64RE.MatchString(tok) && strings.HasSuffix(before, "sha256:") {
		return true
	}
	for _, k := range keep {
		if len(k) >= 7 && tok == k {
			return true
		}
	}
	return false
}

// maskKeyValues masks the value after every secret-looking key.
func maskKeyValues(s string) string {
	var b strings.Builder
	pos := 0
	for pos < len(s) {
		m := keyRE.FindStringSubmatchIndex(s[pos:])
		if m == nil {
			break
		}
		start, end := pos+m[0], pos+m[1]
		key := s[pos+m[4] : pos+m[5]]
		// The key must start a word: "xpassword=" is no key.
		if start > 0 && isWordByte(s[start-1]) && m[2] == m[3] || !isSecretKey(key) {
			// Go on after the key, so that a key in the rest is still found.
			b.WriteString(s[pos : pos+m[5]])
			pos += m[5]
			continue
		}
		b.WriteString(s[pos:end])
		// A cookie or auth value has several parts (a=b; session=..., Digest
		// username="u", response="..."): the rest of the line goes. "auth" covers
		// Authorization, Proxy-Authorization and the short Auth alias (Kimi run 11, third
		// review); masking a little too much (Author:) is the safe side.
		if lk := strings.ToLower(key); strings.Contains(lk, "cookie") || strings.Contains(lk, "auth") {
			b.WriteString(masked)
			return b.String()
		}
		n, all := valueLen(s[end:])
		if all {
			b.WriteString(masked)
			return b.String()
		}
		q := ""
		if n > 0 && (s[end] == '"' || s[end] == '\'') {
			q = s[end : end+1]
		}
		if n > 0 {
			b.WriteString(q + masked + q)
		}
		pos = end + n
	}
	b.WriteString(s[pos:])
	return b.String()
}

func isWordByte(c byte) bool {
	return c == '_' || c == '-' || c == '.' || c >= '0' && c <= '9' || c >= 'a' && c <= 'z' || c >= 'A' && c <= 'Z'
}

// valueLen is the length of the value at the start of v; all is true when the value cannot be
// parsed for sure (an open quote, an escaped quote, a structure), so the rest of the line goes.
func valueLen(v string) (n int, all bool) {
	if v == "" {
		return 0, false
	}
	switch v[0] {
	case '"', '\'':
		for i := 1; i < len(v); i++ {
			switch v[i] {
			case '\\':
				i++
			case v[0]:
				return i + 1, false
			}
		}
		return 0, true
	case '\\', '{', '[', '(', '<':
		return 0, true
	}
	n = wordEnd(v, 0)
	if authSchemes[strings.ToLower(v[:n])] && n < len(v) && (v[n] == ' ' || v[n] == '\t') {
		i := n
		for i < len(v) && (v[i] == ' ' || v[i] == '\t') {
			i++
		}
		if i < len(v) && strings.ContainsRune("\"'\\{[(<", rune(v[i])) {
			return 0, true
		}
		n = wordEnd(v, i)
	}
	return n, false
}

// wordEnd is the index of the first space or tab at or after i.
func wordEnd(v string, i int) int {
	for i < len(v) && v[i] != ' ' && v[i] != '\t' {
		i++
	}
	return i
}

// isSecretKey splits the key into words ("_", "-", ".", camelCase) and looks for a secret word.
func isSecretKey(key string) bool {
	if secretWords[strings.ToLower(strings.NewReplacer("_", "", "-", "", ".", "").Replace(key))] {
		return true
	}
	var words []string
	cur := []rune{}
	rs := []rune(key)
	flush := func() {
		if len(cur) > 0 {
			words = append(words, strings.ToLower(string(cur)))
			cur = cur[:0]
		}
	}
	for i, r := range rs {
		if r == '_' || r == '-' || r == '.' {
			flush()
			continue
		}
		upper := r >= 'A' && r <= 'Z'
		if upper && i > 0 {
			prev := rs[i-1]
			nextLower := i+1 < len(rs) && rs[i+1] >= 'a' && rs[i+1] <= 'z'
			if prev >= 'a' && prev <= 'z' || prev >= '0' && prev <= '9' || (prev >= 'A' && prev <= 'Z' && nextLower) {
				flush()
			}
		}
		cur = append(cur, r)
	}
	flush()
	for _, w := range words {
		if secretWords[w] {
			return true
		}
	}
	return false
}

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

// all is a copy of the lines kept.
func (t *tail) all() []string {
	t.mu.Lock()
	defer t.mu.Unlock()
	return slices.Clone(t.lines)
}

func (t *tail) String() string {
	t.mu.Lock()
	defer t.mu.Unlock()
	return strings.Join(t.lines, "\n")
}

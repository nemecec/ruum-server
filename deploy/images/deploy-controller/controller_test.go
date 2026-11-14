package main

import (
	"context"
	"errors"
	"io"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"
)

// memStore is a Store in memory with resourceVersion checks, like the API server.
type memStore struct {
	mu    sync.Mutex
	items map[string]ConfigMap
	rv    int
}

func newStore() *memStore { return &memStore{items: map[string]ConfigMap{}} }

func clone(cm ConfigMap) ConfigMap {
	out := cm
	out.Metadata.Annotations = map[string]string{}
	for k, v := range cm.Metadata.Annotations {
		out.Metadata.Annotations[k] = v
	}
	out.Data = map[string]string{}
	for k, v := range cm.Data {
		out.Data[k] = v
	}
	return out
}

func (s *memStore) add(name string, created time.Time, data map[string]string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.rv++
	s.items[name] = ConfigMap{Metadata: ObjectMeta{
		Name: name, ResourceVersion: strconv.Itoa(s.rv), CreationTimestamp: created,
		Labels: map[string]string{requestLabel: "true"},
	}, Data: data}
}

func (s *memStore) List(context.Context) ([]ConfigMap, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	var out []ConfigMap
	for _, cm := range s.items {
		out = append(out, clone(cm))
	}
	return out, nil
}

func (s *memStore) Get(_ context.Context, name string) (ConfigMap, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	cm, ok := s.items[name]
	if !ok {
		return ConfigMap{}, ErrNotFound
	}
	return clone(cm), nil
}

func (s *memStore) Update(_ context.Context, cm ConfigMap) (ConfigMap, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	cur, ok := s.items[cm.Metadata.Name]
	if !ok {
		return ConfigMap{}, ErrNotFound
	}
	if cur.Metadata.ResourceVersion != cm.Metadata.ResourceVersion {
		return ConfigMap{}, ErrConflict
	}
	s.rv++
	cm = clone(cm)
	cm.Metadata.ResourceVersion = strconv.Itoa(s.rv)
	s.items[cm.Metadata.Name] = cm
	return clone(cm), nil
}

func (s *memStore) Delete(_ context.Context, name string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.items, name)
	return nil
}

func (s *memStore) phase(name string) string {
	cm, _ := s.Get(context.Background(), name)
	return cm.Metadata.Annotations[annPhase]
}

// fakeDeployer records the runs and prints lines.
type fakeDeployer struct {
	runs  [][]string
	lines []string
	err   error
}

func (f *fakeDeployer) Run(_ context.Context, args []string, line func(string)) error {
	f.runs = append(f.runs, args)
	for _, l := range f.lines {
		line(l)
	}
	return f.err
}

var t0 = time.Date(2026, 11, 13, 12, 0, 0, 0, time.UTC)

func newController(s *memStore, d *fakeDeployer) *Controller {
	return &Controller{
		Store: s, Checker: world(), Deployer: d, Me: "deploy-controller-abc",
		MaxAge: 5 * time.Minute, Keep: 7 * 24 * time.Hour, Timeout: time.Hour, LogLines: 50, ProgressEvery: time.Millisecond,
		Now: func() time.Time { return t0 }, Log: io.Discard,
	}
}

func data(r Request) map[string]string {
	d := map[string]string{"commit": r.Commit, "server": r.Server, "edge": r.Edge, "expectDeployed": r.ExpectDeployed}
	if r.Rollback != "" {
		d["rollback"] = r.Rollback
	}
	return d
}

func TestControllerDeploysAnAcceptedRequest(t *testing.T) {
	s, d := newStore(), &fakeDeployer{lines: []string{
		"==> migration step ruum-migrate-111111111111-202611131200",
		"migrate| connecting to postgres://ruum:s3cret@pg-rw.data:5432/ruum",
		"error: postgres://ruum:s3cret@pg-rw.data:5432/ruum refused",
		"==> migrations are up to date; rolling update",
	}}
	s.add("release-a", t0.Add(-time.Minute), data(req(cNew, cLive, "")))
	c := newController(s, d)
	if err := c.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(d.runs) != 1 || d.runs[0][0] != cNew || !slices.Contains(d.runs[0], "--backup") {
		t.Fatalf("runs %v", d.runs)
	}
	cm, _ := s.Get(context.Background(), "release-a")
	if cm.Metadata.Annotations[annPhase] != PhaseSucceeded || cm.Metadata.Annotations[annClaimedBy] != c.Me {
		t.Fatalf("annotations %v", cm.Metadata.Annotations)
	}
	log := cm.Data[keyLog]
	if strings.Contains(log, "migrate|") {
		t.Fatalf("the migration Job log reached the request: %q", log)
	}
	if strings.Contains(log, "s3cret") || !strings.Contains(log, "postgres://[redacted]@pg-rw.data") {
		t.Fatalf("credentials not masked: %q", log)
	}
	if !strings.Contains(log, "rolling update") || !strings.Contains(cm.Data[keyStatus], "deployed "+cNew) {
		t.Fatalf("status %q log %q", cm.Data[keyStatus], log)
	}
	// Nothing more to do.
	if err := c.Reconcile(context.Background()); err != nil || len(d.runs) != 1 {
		t.Fatalf("second reconcile: %v, runs %d", err, len(d.runs))
	}
}

func TestControllerRefusesWithoutRunning(t *testing.T) {
	cases := map[string]Request{
		"unsigned":     func() Request { r := req(cNew, cLive, ""); r.Server = srv(dBad); return r }(),
		"non-main":     req(cSide, cLive, ""),
		"older":        req(cOld, cLive, ""),
		"before-floor": req(cPre, "none", ""),
		"older-back":   req(cOld, cNew, RollbackOlder),
	}
	for name, r := range cases {
		s, d := newStore(), &fakeDeployer{}
		s.add(name, t0.Add(-time.Minute), data(r))
		if err := newController(s, d).Reconcile(context.Background()); err != nil {
			t.Fatal(err)
		}
		if len(d.runs) != 0 {
			t.Fatalf("%s: deployed %v", name, d.runs)
		}
		if p := s.phase(name); p != PhaseRefused {
			t.Fatalf("%s: phase %q, want Refused", name, p)
		}
	}
	// A signed digest that is not signed: the provenance check refuses it.
	s, d := newStore(), &fakeDeployer{}
	c := newController(s, d)
	r := req(cNew, cLive, "")
	delete(c.Checker.Provenance.(fakeProvenance).signed, r.Server)
	s.add("unsigned-tagged", t0.Add(-time.Minute), data(r))
	_ = c.Reconcile(context.Background())
	cm, _ := s.Get(context.Background(), "unsigned-tagged")
	if len(d.runs) != 0 || cm.Metadata.Annotations[annPhase] != PhaseRefused || !strings.Contains(cm.Data[keyStatus], "provenance") {
		t.Fatalf("runs %v, %v, status %q", d.runs, cm.Metadata.Annotations, cm.Data[keyStatus])
	}
}

func TestControllerRefusesAnExpiredRequest(t *testing.T) {
	s, d := newStore(), &fakeDeployer{}
	s.add("old", t0.Add(-6*time.Minute), data(req(cNew, cLive, "")))
	if err := newController(s, d).Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(d.runs) != 0 || s.phase("old") != PhaseRefused {
		t.Fatalf("runs %v phase %q", d.runs, s.phase("old"))
	}
}

func TestControllerTakesTheOldestFirstAndOneAtATime(t *testing.T) {
	s, d := newStore(), &fakeDeployer{}
	s.add("b", t0.Add(-1*time.Minute), data(req(cNew, cLive, "")))
	s.add("a", t0.Add(-2*time.Minute), data(req(cLive, cOld, "")))
	c := newController(s, d)
	_ = c.Reconcile(context.Background())
	if len(d.runs) != 1 || d.runs[0][0] != cLive || s.phase("b") != "" {
		t.Fatalf("runs %v, b %q", d.runs, s.phase("b"))
	}
	_ = c.Reconcile(context.Background())
	if len(d.runs) != 2 || d.runs[1][0] != cNew {
		t.Fatalf("runs %v", d.runs)
	}
}

func TestControllerMarksAFailedDeploy(t *testing.T) {
	s, d := newStore(), &fakeDeployer{err: errors.New("exit status 1"), lines: []string{"deploy-chart: the migration step did not succeed"}}
	s.add("x", t0.Add(-time.Minute), data(req(cNew, cLive, "")))
	_ = newController(s, d).Reconcile(context.Background())
	cm, _ := s.Get(context.Background(), "x")
	if cm.Metadata.Annotations[annPhase] != PhaseFailed || !strings.Contains(cm.Data[keyLog], "did not succeed") {
		t.Fatalf("%v %q", cm.Metadata.Annotations, cm.Data[keyLog])
	}
}

func TestControllerFailsRequestsOfAGonePod(t *testing.T) {
	s, d := newStore(), &fakeDeployer{}
	s.add("x", t0.Add(-time.Minute), data(req(cNew, cLive, "")))
	cm, _ := s.Get(context.Background(), "x")
	cm.Metadata.Annotations = map[string]string{annPhase: PhaseRunning, annClaimedBy: "deploy-controller-old"}
	if _, err := s.Update(context.Background(), cm); err != nil {
		t.Fatal(err)
	}
	c := newController(s, d)
	if err := c.RecoverInterrupted(context.Background()); err != nil {
		t.Fatal(err)
	}
	if s.phase("x") != PhaseFailed {
		t.Fatalf("phase %q", s.phase("x"))
	}
	_ = c.Reconcile(context.Background())
	if len(d.runs) != 0 {
		t.Fatalf("an interrupted request ran again: %v", d.runs)
	}
}

func TestControllerSkipsARequestClaimedByAnother(t *testing.T) {
	s, d := newStore(), &fakeDeployer{}
	s.add("x", t0.Add(-time.Minute), data(req(cNew, cLive, "")))
	c := newController(s, d)
	c.Store = racingStore{s}
	if err := c.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(d.runs) != 0 {
		t.Fatalf("ran a request that another controller claimed: %v", d.runs)
	}
}

// racingStore: another controller claims every request between our List and Update.
type racingStore struct{ *memStore }

func (r racingStore) Update(ctx context.Context, cm ConfigMap) (ConfigMap, error) {
	cur, _ := r.memStore.Get(ctx, cm.Metadata.Name)
	cur.Metadata.Annotations[annPhase] = PhaseRunning
	cur.Metadata.Annotations[annClaimedBy] = "other"
	if _, err := r.memStore.Update(ctx, cur); err != nil {
		return ConfigMap{}, err
	}
	return r.memStore.Update(ctx, cm)
}

func TestControllerDeletesOldFinishedRequests(t *testing.T) {
	s, d := newStore(), &fakeDeployer{}
	s.add("done", t0.Add(-8*24*time.Hour), data(req(cNew, cLive, "")))
	cm, _ := s.Get(context.Background(), "done")
	cm.Metadata.Annotations = map[string]string{annPhase: PhaseSucceeded}
	_, _ = s.Update(context.Background(), cm)
	_ = newController(s, d).Reconcile(context.Background())
	if _, err := s.Get(context.Background(), "done"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("the old request is still there: %v", err)
	}
}

func TestRedact(t *testing.T) {
	for in, want := range map[string]string{
		"dsn postgres://ruum:pw@pg:5432/ruum ok": "dsn postgres://[redacted]@pg:5432/ruum ok",
		"https://user@example.com/x":             "https://[redacted]@example.com/x",
		"https://example.com/a@b":                "https://example.com/a@b",
		"no url here":                            "no url here",
	} {
		if got := redact(in); got != want {
			t.Errorf("redact(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestControllerSkipsARequestItCannotClaim(t *testing.T) {
	s, d := newStore(), &fakeDeployer{}
	s.add("a-immutable", t0.Add(-2*time.Minute), data(req(cLive, cOld, "")))
	s.add("b-good", t0.Add(-time.Minute), data(req(cNew, cLive, "")))
	c := newController(s, d)
	c.Store = immutableStore{s, "a-immutable"}
	if err := c.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(d.runs) != 1 || d.runs[0][0] != cNew {
		t.Fatalf("runs %v: the good request must run past the one that cannot be claimed", d.runs)
	}
}

// immutableStore refuses every update of one request (an immutable ConfigMap).
type immutableStore struct {
	*memStore
	name string
}

func (s immutableStore) Update(ctx context.Context, cm ConfigMap) (ConfigMap, error) {
	if cm.Metadata.Name == s.name {
		return ConfigMap{}, errors.New("HTTP 422: field is immutable")
	}
	return s.memStore.Update(ctx, cm)
}

// slowDeployer prints a line, then waits until the request shows it.
type slowDeployer struct {
	store *memStore
	name  string
	seen  bool
}

func (d *slowDeployer) Run(_ context.Context, _ []string, line func(string)) error {
	line("==> migration step ruum-migrate-x")
	for range 500 {
		cm, _ := d.store.Get(context.Background(), d.name)
		if strings.Contains(cm.Data[keyLog], "migration step") {
			d.seen = true
			return nil
		}
		time.Sleep(2 * time.Millisecond)
	}
	return nil
}

func TestControllerWritesProgressWhileADeployRuns(t *testing.T) {
	s := newStore()
	s.add("x", t0.Add(-time.Minute), data(req(cNew, cLive, "")))
	d := &slowDeployer{store: s, name: "x"}
	c := newController(s, &fakeDeployer{})
	c.Deployer = d
	_ = c.Reconcile(context.Background())
	if !d.seen {
		t.Fatal("the request did not show the output while the deploy ran")
	}
}

// A request that the deployer created with a preset phase (or a claim in the name of this very
// pod) is never run, and it goes after Keep like every other request (review of O-34, change 1).
func TestControllerNeverRunsAPresetPhaseAndDeletesItAfterKeep(t *testing.T) {
	presets := map[string]map[string]string{
		"running-me":  {annPhase: PhaseRunning, annClaimedBy: "deploy-controller-abc"},
		"bogus":       {annPhase: "Bogus"},
		"claimed-me":  {annClaimedBy: "deploy-controller-abc", annPhase: PhaseRunning, annStarted: "2026-11-13T11:59:00Z"},
		"succeeded-x": {annPhase: PhaseSucceeded},
	}
	s, d := newStore(), &fakeDeployer{}
	for name, ann := range presets {
		s.add(name, t0.Add(-time.Minute), data(req(cNew, cLive, "")))
		cm, _ := s.Get(context.Background(), name)
		cm.Metadata.Annotations = ann
		if _, err := s.Update(context.Background(), cm); err != nil {
			t.Fatal(err)
		}
	}
	c := newController(s, d)
	if err := c.RecoverInterrupted(context.Background()); err != nil {
		t.Fatal(err)
	}
	for range 3 {
		if err := c.Reconcile(context.Background()); err != nil {
			t.Fatal(err)
		}
	}
	if len(d.runs) != 0 {
		t.Fatalf("a request with a preset phase ran: %v", d.runs)
	}
	if len(s.items) != len(presets) {
		t.Fatalf("a young request was deleted: %d left", len(s.items))
	}
	// A week and a bit later every one of them is gone.
	c.Now = func() time.Time { return t0.Add(c.Keep + time.Hour) }
	if err := c.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(s.items) != 0 || len(d.runs) != 0 {
		t.Fatalf("left %v, runs %v", s.items, d.runs)
	}
}

// A request that cannot be claimed (immutable) is deleted after Keep too (Kimi's note on O-34).
func TestControllerDeletesAnUnclaimableRequestAfterKeep(t *testing.T) {
	s, d := newStore(), &fakeDeployer{}
	s.add("immutable", t0.Add(-time.Minute), data(req(cNew, cLive, "")))
	c := newController(s, d)
	c.Store = immutableStore{s, "immutable"}
	_ = c.Reconcile(context.Background())
	if len(d.runs) != 0 || len(s.items) != 1 {
		t.Fatalf("runs %v, items %d", d.runs, len(s.items))
	}
	c.Now = func() time.Time { return t0.Add(c.Keep + time.Hour) }
	_ = c.Reconcile(context.Background())
	if len(s.items) != 0 {
		t.Fatal("the request that cannot be claimed is still there after Keep")
	}
}

func TestControllerKeepsOnlyTheNewestFinishedRequests(t *testing.T) {
	s, d := newStore(), &fakeDeployer{}
	for i := range 5 {
		name := "done-" + strconv.Itoa(i)
		s.add(name, t0.Add(-time.Duration(10-i)*time.Hour), data(req(cNew, cLive, "")))
		cm, _ := s.Get(context.Background(), name)
		cm.Metadata.Annotations = map[string]string{annPhase: PhaseFailed}
		_, _ = s.Update(context.Background(), cm)
	}
	s.add("fresh", t0.Add(-time.Minute), nil) // no phase: not counted, refused for its fields
	c := newController(s, d)
	c.KeepCount = 2
	_ = c.Reconcile(context.Background())
	for _, gone := range []string{"done-0", "done-1", "done-2"} {
		if _, err := s.Get(context.Background(), gone); !errors.Is(err, ErrNotFound) {
			t.Fatalf("%s is still there", gone)
		}
	}
	for _, kept := range []string{"done-3", "done-4", "fresh"} {
		if _, err := s.Get(context.Background(), kept); err != nil {
			t.Fatalf("%s was deleted", kept)
		}
	}
}

func TestControllerNamesItsCommitInTheRequestLog(t *testing.T) {
	s, d := newStore(), &fakeDeployer{}
	s.add("x", t0.Add(-time.Minute), data(req(cNew, cLive, "")))
	c := newController(s, d)
	c.Commit = cLive
	_ = c.Reconcile(context.Background())
	cm, _ := s.Get(context.Background(), "x")
	if !strings.Contains(cm.Data[keyLog], "deploy controller of commit "+cLive) {
		t.Fatalf("log %q", cm.Data[keyLog])
	}
}

// cancellingDeployer stops the pod (cancels the controller's context) in the middle of a deploy.
type cancellingDeployer struct {
	cancel    context.CancelFunc
	cancelled bool
}

func (d *cancellingDeployer) Run(ctx context.Context, _ []string, line func(string)) error {
	d.cancel()
	time.Sleep(5 * time.Millisecond)
	d.cancelled = ctx.Err() != nil
	line("==> rolling update done")
	return ctx.Err()
}

// A stop of the pod (SIGTERM) lets the running deploy finish and writes its outcome, and takes no
// new request (review of O-34, change 3).
func TestControllerFinishesARunningDeployOnStop(t *testing.T) {
	s := newStore()
	s.add("a", t0.Add(-2*time.Minute), data(req(cNew, cLive, "")))
	s.add("b", t0.Add(-time.Minute), data(req(cNew, cLive, "")))
	ctx, cancel := context.WithCancel(context.Background())
	d := &cancellingDeployer{cancel: cancel}
	c := newController(s, &fakeDeployer{})
	c.Deployer = d
	if err := c.Reconcile(ctx); err != nil {
		t.Fatal(err)
	}
	if d.cancelled {
		t.Fatal("the stop cancelled the running deploy")
	}
	if p := s.phase("a"); p != PhaseSucceeded {
		t.Fatalf("phase of the running request %q, want Succeeded", p)
	}
	if err := c.Reconcile(ctx); err != nil {
		t.Fatal(err)
	}
	if p := s.phase("b"); p != "" {
		t.Fatalf("a new request was taken after the stop: %q", p)
	}
}

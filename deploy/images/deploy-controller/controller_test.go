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
		MaxAge: 5 * time.Minute, Keep: 7 * 24 * time.Hour, Timeout: time.Hour, LogLines: 50,
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

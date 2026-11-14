package main

import (
	"context"
	"errors"
	"slices"
	"strings"
	"testing"
)

const (
	cNew  = "1111111111111111111111111111111111111111" // the target of most tests, on main
	cLive = "2222222222222222222222222222222222222222" // the live release, older than cNew
	cOld  = "3333333333333333333333333333333333333333" // older than cLive
	cSide = "4444444444444444444444444444444444444444" // a branch commit, not on main
	cPre  = "5555555555555555555555555555555555555555" // on main, before O-25
	dSrv  = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	dEdge = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
	dBad  = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
)

// fakeRegistry: tags[image+":"+tag] = digest, revisions[ref] = commit.
type fakeRegistry struct {
	tags      map[string]string
	revisions map[string]string
}

func (f fakeRegistry) TagDigest(_ context.Context, image, tag string) (string, error) {
	d, ok := f.tags[image+":"+tag]
	if !ok {
		return "", errors.New("no tag")
	}
	return d, nil
}

func (f fakeRegistry) Revision(_ context.Context, ref string) (string, error) {
	return f.revisions[ref], nil
}

// fakeProvenance accepts only the signed refs.
type fakeProvenance struct{ signed map[string]bool }

func (f fakeProvenance) Verify(_ context.Context, ref string) error {
	if !f.signed[ref] {
		return errors.New("no matching attestations")
	}
	return nil
}

// fakeGit: main is cOld -> cLive -> cNew (linear); cSide branches off cLive; cPre is before O-25.
type fakeGit struct {
	files map[string][]string // changed files between two commits, key "a..b" (a older)
}

var mainOrder = map[string]int{cPre: 0, cOld: 1, cLive: 2, cNew: 3}

func (f fakeGit) Compare(_ context.Context, base, head string) (Comparison, error) {
	if head == "main" {
		head = cNew
	}
	bi, bok := mainOrder[base]
	hi, hok := mainOrder[head]
	switch {
	case !bok || !hok:
		return Comparison{Status: "diverged"}, nil
	case bi == hi:
		return Comparison{Status: "identical"}, nil
	case hi > bi:
		return Comparison{Status: "ahead", Files: f.files[base+".."+head]}, nil
	default:
		return Comparison{Status: "behind"}, nil
	}
}

func (f fakeGit) HasFile(_ context.Context, commit, _ string) (bool, error) {
	return commit != cPre, nil
}

func srv(d string) string  { return "ghcr.io/nemecec/ruum-server@" + d }
func edge(d string) string { return "ghcr.io/nemecec/ruum-edge@" + d }

// world: every commit has signed images tagged with it, built from it.
func world() Checker {
	reg := fakeRegistry{tags: map[string]string{}, revisions: map[string]string{}}
	prov := fakeProvenance{signed: map[string]bool{}}
	for _, c := range []string{cNew, cLive, cOld, cSide, cPre} {
		s, e := dSrv[:len(dSrv)-1]+c[:1], dEdge[:len(dEdge)-1]+c[:1]
		reg.tags["nemecec/ruum-server:"+c] = s
		reg.tags["nemecec/ruum-edge:"+c] = e
		reg.revisions[srv(s)] = c
		reg.revisions[edge(e)] = c
		prov.signed[srv(s)] = true
		prov.signed[edge(e)] = true
	}
	return Checker{Registry: reg, Provenance: prov, Git: fakeGit{files: map[string][]string{
		cLive + ".." + cNew: {"crates/ruum-api/src/lib.rs", "migrations/0042_x.sql"},
		cOld + ".." + cLive: {"web/src/x.ts"},
		cOld + ".." + cNew:  {"web/src/x.ts", "migrations/0042_x.sql"},
	}}}
}

// req: a request for commit c with its own images.
func req(c, live, rollback string) Request {
	return Request{
		Commit: c, Server: srv(dSrv[:len(dSrv)-1] + c[:1]), Edge: edge(dEdge[:len(dEdge)-1] + c[:1]),
		ExpectDeployed: live, Rollback: rollback,
	}
}

func mustRefuse(t *testing.T, err error, want string) {
	t.Helper()
	if err == nil {
		t.Fatalf("accepted; want a refusal with %q", want)
	}
	if !IsRefusal(err) {
		t.Fatalf("error %v is not a refusal", err)
	}
	if !strings.Contains(err.Error(), want) {
		t.Fatalf("refusal %q does not contain %q", err, want)
	}
}

func TestForwardDeployWithMigrationsTakesABackup(t *testing.T) {
	p, err := world().Check(context.Background(), req(cNew, cLive, RollbackNone))
	if err != nil {
		t.Fatal(err)
	}
	want := []string{cNew, req(cNew, "", "").Server, req(cNew, "", "").Edge, "--expect-deployed", cLive, "--backup"}
	if !slices.Equal(p.Args, want) {
		t.Fatalf("args %v, want %v", p.Args, want)
	}
}

func TestForwardDeployWithoutMigrationsTakesNoBackup(t *testing.T) {
	p, err := world().Check(context.Background(), req(cLive, cOld, RollbackNone))
	if err != nil {
		t.Fatal(err)
	}
	if slices.Contains(p.Args, "--backup") {
		t.Fatalf("args %v have --backup", p.Args)
	}
}

func TestFirstDeployAndRedeploy(t *testing.T) {
	for _, live := range []string{"none", cNew} {
		p, err := world().Check(context.Background(), req(cNew, live, RollbackNone))
		if err != nil {
			t.Fatalf("live %s: %v", live, err)
		}
		if slices.Contains(p.Args, "--backup") || slices.Contains(p.Args, "--rollback") {
			t.Fatalf("live %s: args %v", live, p.Args)
		}
	}
}

func TestRefusesAnUnsignedDigest(t *testing.T) {
	w := world()
	r := req(cNew, cLive, RollbackNone)
	delete(w.Provenance.(fakeProvenance).signed, r.Edge)
	_, err := w.Check(context.Background(), r)
	mustRefuse(t, err, "no valid provenance attestation")
}

func TestRefusesADigestThatIsNotTheTag(t *testing.T) {
	r := req(cNew, cLive, RollbackNone)
	r.Server = srv(dBad)
	_, err := world().Check(context.Background(), r)
	mustRefuse(t, err, "is "+req(cNew, "", "").Server[len("ghcr.io/nemecec/ruum-server@"):])
}

func TestRefusesAnImageBuiltFromAnotherCommit(t *testing.T) {
	w := world()
	r := req(cNew, cLive, RollbackNone)
	w.Registry.(fakeRegistry).revisions[r.Server] = cSide
	_, err := w.Check(context.Background(), r)
	mustRefuse(t, err, "was built from")
}

func TestRefusesANonMainCommit(t *testing.T) {
	_, err := world().Check(context.Background(), req(cSide, cLive, RollbackNone))
	mustRefuse(t, err, "is not on main")
}

func TestRefusesACommitBeforeTheFloor(t *testing.T) {
	_, err := world().Check(context.Background(), req(cPre, "none", RollbackNone))
	mustRefuse(t, err, "older than backlog O-25")
}

func TestRefusesAnOlderCommitWithoutRollback(t *testing.T) {
	_, err := world().Check(context.Background(), req(cOld, cLive, RollbackNone))
	mustRefuse(t, err, "pass --rollback")
}

func TestRollbackToAnOlderCommitPassesTheMode(t *testing.T) {
	p, err := world().Check(context.Background(), req(cOld, cLive, RollbackPrevious))
	if err != nil {
		t.Fatal(err)
	}
	// deploy-chart.sh checks, under the deploy lock, that cOld is `previous` in ruum-release.
	if !slices.Contains(p.Args, "--rollback") || slices.Contains(p.Args, "--rollback-older") {
		t.Fatalf("args %v", p.Args)
	}
	if slices.Contains(p.Args, "--backup") {
		t.Fatalf("no migrations between cOld and cLive, but args %v", p.Args)
	}
	// Newer migrations between the target and the live release: a backup first.
	p, err = world().Check(context.Background(), req(cOld, cNew, RollbackPrevious))
	if err != nil {
		t.Fatal(err)
	}
	if !slices.Contains(p.Args, "--rollback") || !slices.Contains(p.Args, "--backup") {
		t.Fatalf("args %v: want --rollback and --backup (migrations between cOld and cNew)", p.Args)
	}
}

// Kimi review of O-34, finding 1: the deployer credential (maybe with an agent or CI) never
// gets a rollback past the previous release, whatever the target; the admin runs it.
func TestRefusesARollbackOlder(t *testing.T) {
	for _, r := range []Request{
		req(cOld, cNew, RollbackOlder),  // two releases back, migrations in between
		req(cOld, cLive, RollbackOlder), // one back: --rollback is the way
	} {
		w := world()
		w.Registry, w.Git = panicRegistry{}, panicGit{}
		p, err := w.Check(context.Background(), r)
		mustRefuse(t, err, "does not run it")
		if p.Args != nil {
			t.Fatalf("a refusal with a plan: %v", p.Args)
		}
	}
}

// panicRegistry and panicGit: a refused rollback older reads nothing (no GitHub call spent).
type panicRegistry struct{}

func (panicRegistry) TagDigest(context.Context, string, string) (string, error) {
	panic("registry read for a rollback older")
}
func (panicRegistry) Revision(context.Context, string) (string, error) {
	panic("registry read for a rollback older")
}

type panicGit struct{}

func (panicGit) Compare(context.Context, string, string) (Comparison, error) {
	panic("GitHub read for a rollback older")
}
func (panicGit) HasFile(context.Context, string, string) (bool, error) {
	panic("GitHub read for a rollback older")
}

func TestRefusesARollbackToANewerCommit(t *testing.T) {
	_, err := world().Check(context.Background(), req(cNew, cLive, RollbackPrevious))
	mustRefuse(t, err, "is newer than the live")
}

func TestRefusesARollbackToTheLiveCommit(t *testing.T) {
	_, err := world().Check(context.Background(), req(cLive, cLive, RollbackPrevious))
	mustRefuse(t, err, "is the live release")
}

func TestRefusesALiveReleaseOffMain(t *testing.T) {
	_, err := world().Check(context.Background(), req(cNew, cSide, RollbackNone))
	mustRefuse(t, err, "not on one line of main")
}

func TestTruncatedFileListTakesABackup(t *testing.T) {
	w := world()
	g := w.Git.(fakeGit)
	g.files[cOld+".."+cLive] = nil
	w.Git = truncGit{g}
	p, err := w.Check(context.Background(), req(cLive, cOld, RollbackNone))
	if err != nil {
		t.Fatal(err)
	}
	if !slices.Contains(p.Args, "--backup") {
		t.Fatalf("args %v: want --backup when GitHub cut the file list", p.Args)
	}
}

type truncGit struct{ fakeGit }

func (g truncGit) Compare(ctx context.Context, base, head string) (Comparison, error) {
	c, err := g.fakeGit.Compare(ctx, base, head)
	c.Truncated = true
	return c, err
}

func TestTransientErrorsAreNotRefusals(t *testing.T) {
	w := world()
	w.Git = failingGit{}
	_, err := w.Check(context.Background(), req(cNew, cLive, RollbackNone))
	if err == nil || IsRefusal(err) {
		t.Fatalf("want a non-refusal error, got %v", err)
	}
}

type failingGit struct{}

func (failingGit) Compare(context.Context, string, string) (Comparison, error) {
	return Comparison{}, errors.New("rate limited")
}
func (failingGit) HasFile(context.Context, string, string) (bool, error) { return false, nil }

func TestAcmeArgs(t *testing.T) {
	r := req(cNew, cLive, RollbackNone)
	r.Acme, r.AcmeEmail = "zerossl", "ops@example.com"
	p, err := world().Check(context.Background(), r)
	if err != nil {
		t.Fatal(err)
	}
	n := len(p.Args)
	if !slices.Equal(p.Args[n-4:], []string{"--acme", "zerossl", "--acme-email", "ops@example.com"}) {
		t.Fatalf("args %v", p.Args)
	}
}

func TestParseRequest(t *testing.T) {
	good := map[string]string{
		"commit": cNew, "server": srv(dSrv), "edge": edge(dEdge), "expectDeployed": cLive, "rollback": "previous",
	}
	if _, err := ParseRequest(good); err != nil {
		t.Fatal(err)
	}
	bad := []struct {
		key, value, want string
	}{
		{"commit", "abc", "40 lowercase hex"},
		{"commit", strings.Repeat("A", 40), "40 lowercase hex"},
		{"server", edge(dEdge), "server image"},
		{"server", "ghcr.io/nemecec/ruum-server:main", "server image"},
		{"server", "docker.io/library/busybox@" + dSrv, "server image"},
		{"edge", srv(dSrv), "edge image"},
		{"expectDeployed", "", "expectDeployed"},
		{"rollback", "all", "rollback must be"},
		{"acme", "letsencrypt", "acme must be"},
		{"acmeEmail", "a@b.c,x=y", "acmeEmail"},
		{"status", "Succeeded", "unknown keys"},
		{"values", "image: x", "unknown keys"},
	}
	for _, b := range bad {
		d := map[string]string{}
		for k, v := range good {
			d[k] = v
		}
		d[b.key] = b.value
		_, err := ParseRequest(d)
		mustRefuse(t, err, b.want)
	}
	d := map[string]string{"commit": cNew, "server": srv(dSrv), "edge": edge(dEdge), "expectDeployed": "none", "rollback": "previous"}
	_, err := ParseRequest(d)
	mustRefuse(t, err, "needs a live release")
	d = map[string]string{"commit": cNew, "server": srv(dSrv), "edge": edge(dEdge), "expectDeployed": "none", "acme": "zerossl"}
	_, err = ParseRequest(d)
	mustRefuse(t, err, "needs acmeEmail")
}

package main

import (
	"context"
	"errors"
	"fmt"
	"strings"
)

// Registry reads the image registry (GHCR).
type Registry interface {
	// TagDigest is the manifest digest of image:tag (image like "nemecec/ruum-server").
	TagDigest(ctx context.Context, image, tag string) (string, error)
	// Revision is the label org.opencontainers.image.revision of the image ref (by digest).
	Revision(ctx context.Context, ref string) (string, error)
}

// Provenance verifies the provenance attestation of an image ref.
type Provenance interface {
	Verify(ctx context.Context, ref string) error
}

// Comparison is the answer of GitHub's compare base...head.
type Comparison struct {
	// Status is ahead (head is newer than base), behind, identical or diverged.
	Status string
	// Files are the changed paths between the merge base and head.
	Files []string
	// Truncated is true when GitHub left files out (more than 300).
	Truncated bool
}

// Git reads the history of the repository (GitHub's API).
type Git interface {
	Compare(ctx context.Context, base, head string) (Comparison, error)
	HasFile(ctx context.Context, commit, path string) (bool, error)
}

// floorFiles: a commit without them is older than backlog O-25 (no migration step, no edge
// certificate storage); deploy/staging/ops/release-floor.sh has the same list.
var floorFiles = []string{
	"crates/ruum-server/src/migrate.rs",
	"deploy/images/caddy-secret-storage/storage.go",
}

// RollbackOlderRefusal is the reason for a request with rollback "older" (Kimi review of O-34,
// finding 1). An older image ignores newer migrations that it does not know, so the migration Job
// does not stop a release that cannot run on the current schema; only a person who checked every
// migration since the target may go back past the previous release. The deployer credential may
// be with an agent or CI, so the controller never runs such a rollback; the admin runs it in the
// controller's pod (deploy/staging/README.md, "Going back past the previous release"). This stays
// until backlog O-37 (schema_floor) lets the controller check the schema itself.
const RollbackOlderRefusal = "rollback older goes back past the previous release, and the deploy controller does not run it: design 12.7 only promises that the previous release runs on the current schema. An admin runs it in the controller's pod after checking every migration since the target (deploy/staging/README.md, \"Going back past the previous release\"; backlog O-37)"

// Plan is what the controller runs for an accepted request: the arguments of deploy-chart.sh.
type Plan struct {
	Args  []string
	Notes []string
}

// Checker decides whether a request may run. Each check is the controller's own; nothing that
// the deployer says about provenance, history or migrations is trusted.
type Checker struct {
	Registry   Registry
	Provenance Provenance
	Git        Git
}

// Check runs every check of a request and returns the plan, a *Refusal, or another error (a
// registry or GitHub failure; the request fails and the person runs deploy.sh again).
func (c Checker) Check(ctx context.Context, r Request) (Plan, error) {
	// 0. Never a rollback past the previous release (Kimi review of O-34, finding 1).
	if r.Rollback == RollbackOlder {
		return Plan{}, refuse("%s", RollbackOlderRefusal)
	}
	var notes []string
	// 1. Each image: the tag <commit> is this digest, the digest has a provenance attestation of
	//    images.yml on refs/heads/main (cosign), and the attested image says it was built from
	//    this commit (its revision label is part of the signed digest).
	for _, ref := range []string{r.Server, r.Edge} {
		image, digest, _ := strings.Cut(strings.TrimPrefix(ref, "ghcr.io/"), "@")
		tagged, err := c.Registry.TagDigest(ctx, image, r.Commit)
		if err != nil {
			return Plan{}, fmt.Errorf("reading the tag ghcr.io/%s:%s: %w", image, r.Commit, err)
		}
		if tagged != digest {
			return Plan{}, refuse("ghcr.io/%s:%s is %s, not %s", image, r.Commit, tagged, digest)
		}
		if err := c.Provenance.Verify(ctx, ref); err != nil {
			return Plan{}, refuse("no valid provenance attestation of images.yml on refs/heads/main for %s: %v", ref, err)
		}
		rev, err := c.Registry.Revision(ctx, ref)
		if err != nil {
			return Plan{}, fmt.Errorf("reading the revision label of %s: %w", ref, err)
		}
		if rev != r.Commit {
			return Plan{}, refuse("%s was built from %q, not from %s", ref, rev, r.Commit)
		}
	}
	// 2. The commit is on main.
	onMain, err := c.Git.Compare(ctx, r.Commit, "main")
	if err != nil {
		return Plan{}, fmt.Errorf("comparing %s with main: %w", r.Commit, err)
	}
	if onMain.Status != "ahead" && onMain.Status != "identical" {
		return Plan{}, refuse("%s is not on main (GitHub compare: %s)", r.Commit, onMain.Status)
	}
	// 3. The floor: O-25 or later.
	for _, f := range floorFiles {
		ok, err := c.Git.HasFile(ctx, r.Commit, f)
		if err != nil {
			return Plan{}, fmt.Errorf("reading %s at %s: %w", f, r.Commit, err)
		}
		if !ok {
			return Plan{}, refuse("%s is older than backlog O-25: it has no migration step (ruum-server migrate) and no edge certificate storage, so this deploy path cannot run it", r.Commit)
		}
	}
	args := []string{r.Commit, r.Server, r.Edge, "--expect-deployed", r.ExpectDeployed}
	// 4. The order against the live release that deploy.sh read (deploy-chart.sh checks under
	//    the deploy lock that it is still live, and that --rollback goes to `previous`).
	backup := false
	if r.ExpectDeployed != "none" && r.ExpectDeployed != r.Commit {
		cmp, err := c.Git.Compare(ctx, r.ExpectDeployed, r.Commit)
		if err != nil {
			return Plan{}, fmt.Errorf("comparing the live %s with %s: %w", r.ExpectDeployed, r.Commit, err)
		}
		files := cmp
		switch cmp.Status {
		case "ahead":
			if r.Rollback != RollbackNone {
				return Plan{}, refuse("a rollback was asked, but %s is newer than the live %s", r.Commit, r.ExpectDeployed)
			}
		case "behind":
			switch r.Rollback {
			case RollbackNone:
				return Plan{}, refuse("%s is older than the live %s; pass --rollback to go back one release on purpose", r.Commit, r.ExpectDeployed)
			case RollbackPrevious:
				args = append(args, "--rollback")
			}
			// Three-dot compare shows the changes since the merge base, which is the target
			// itself here; the other direction lists the migrations between the two.
			files, err = c.Git.Compare(ctx, r.Commit, r.ExpectDeployed)
			if err != nil {
				return Plan{}, fmt.Errorf("comparing %s with the live %s: %w", r.Commit, r.ExpectDeployed, err)
			}
		case "identical":
		default:
			return Plan{}, refuse("%s and the live %s are not on one line of main (GitHub compare: %s)", r.Commit, r.ExpectDeployed, cmp.Status)
		}
		// 5. New (or, for a rollback, newer) migrations: an on-demand backup first (review 2 U-3).
		//    When GitHub cut the list, take the backup.
		backup = files.Truncated
		for _, f := range files.Files {
			if strings.HasPrefix(f, "migrations/") {
				backup = true
			}
		}
	} else if r.Rollback != RollbackNone && r.ExpectDeployed == r.Commit {
		return Plan{}, refuse("a rollback was asked, but %s is the live release", r.Commit)
	}
	if backup {
		args = append(args, "--backup")
		notes = append(notes, "migrations changed since the live release: an on-demand backup first")
	}
	if r.Acme != "" {
		args = append(args, "--acme", r.Acme)
	}
	if r.AcmeEmail != "" {
		args = append(args, "--acme-email", r.AcmeEmail)
	}
	return Plan{Args: args, Notes: notes}, nil
}

// IsRefusal reports whether err is a refusal of the request (not a transient failure).
func IsRefusal(err error) bool {
	var r *Refusal
	return errors.As(err, &r)
}

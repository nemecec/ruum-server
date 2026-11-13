// Command deploy-controller is the deploy controller of Ruum staging (backlog O-34, decision
// 0008-2, option a). It runs in its own namespace (ruum-deploy) with its own ServiceAccount.
//
// The deployer may only create release requests: ConfigMaps with the label
// ruum.space/release-request=true in ruum-deploy, holding the commit, the two image digests,
// the live commit it read and the rollback mode (deploy/staging/ops/release-request.sh). The
// controller takes them one at a time, oldest first, and:
//
//  1. checks every field (the same patterns as ops/image-check.sh);
//  2. checks each image itself: the tag <commit> is that digest, the digest has a provenance
//     attestation of images.yml on refs/heads/main (cosign), and the signed image's revision
//     label is the commit;
//  3. checks with GitHub's API that the commit is on main, that it has the migration step and the
//     edge storage (the floor of backlog O-25), and its order against the live release: an
//     older commit needs a rollback, and the migrations in between decide the backup;
//  4. runs ops/deploy-chart.sh in this pod: the deploy lock, the live release check, the rollback
//     rule (previous release only unless --rollback-older), the backup, the migration Job
//     (owner DSN from ruum-migrate-db, which the deployer cannot reach) and helm upgrade;
//  5. writes the phase (annotation ruum.space/phase), a status line and the output (without the
//     migration Job's log, URLs masked) back into the request.
//
// Only the Go standard library; kubectl, helm, cosign, bash, curl and jq are in the image.
package main

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"
)

func env(name, def string) string {
	if v := os.Getenv(name); v != "" {
		return v
	}
	return def
}

// commitEnv is RUUM_CONTROLLER_COMMIT when it is a full commit SHA.
func commitEnv() string {
	if v := os.Getenv("RUUM_CONTROLLER_COMMIT"); commitRE.MatchString(v) {
		return v
	}
	return ""
}

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	if err := run(ctx); err != nil {
		fmt.Fprintf(os.Stderr, "deploy-controller: %v\n", err)
		os.Exit(1)
	}
}

func run(ctx context.Context) error {
	kube, err := InCluster()
	if err != nil {
		return err
	}
	web := &http.Client{Timeout: 60 * time.Second}
	c := &Controller{
		Store: Requests{Kube: kube, Namespace: env("RUUM_REQUEST_NAMESPACE", "ruum-deploy")},
		Checker: Checker{
			Registry:   GHCR{Base: "https://ghcr.io", HTTP: web},
			Provenance: Cosign{Path: env("RUUM_COSIGN", "cosign")},
			Git:        GitHub{Base: "https://api.github.com", Repo: "nemecec/ruum-server", HTTP: web},
		},
		Deployer: Script{
			Path: env("RUUM_DEPLOY_SCRIPT", "/opt/ruum/deploy/staging/ops/deploy-chart.sh"),
			Env:  []string{"RUUM_DEPLOY_IN_CLUSTER=1"},
		},
		Me:            env("POD_NAME", "deploy-controller"),
		MaxAge:        5 * time.Minute,
		Keep:          7 * 24 * time.Hour,
		KeepCount:     50,
		Commit:        commitEnv(),
		Timeout:       55 * time.Minute,
		LogLines:      300,
		ProgressEvery: 10 * time.Second,
		Now:           time.Now,
		Log:           os.Stdout,
	}
	c.logf("started as %s (commit %s); watching release requests in %s", c.Me, c.Commit, env("RUUM_REQUEST_NAMESPACE", "ruum-deploy"))
	go func() {
		<-ctx.Done()
		c.logf("stop requested: no new request is taken; a running deploy finishes first")
	}()
	for {
		if err := c.RecoverInterrupted(ctx); err == nil {
			break
		} else {
			c.logf("cannot list the requests yet: %v", err)
		}
		select {
		case <-ctx.Done():
			return nil
		case <-time.After(5 * time.Second):
		}
	}
	for {
		if err := c.Reconcile(ctx); err != nil {
			c.logf("reconcile: %v", err)
		}
		select {
		case <-ctx.Done():
			c.logf("stopping")
			return nil
		case <-time.After(5 * time.Second):
		}
	}
}

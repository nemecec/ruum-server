package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func digestOf(b []byte) string {
	s := sha256.Sum256(b)
	return "sha256:" + hex.EncodeToString(s[:])
}

// fakeGHCR serves one image index (amd64 image plus an attestation manifest) under tag cNew.
func fakeGHCR(t *testing.T, revision string, tamper bool) (*httptest.Server, string) {
	t.Helper()
	cfg, _ := json.Marshal(map[string]any{"config": map[string]any{"Labels": map[string]string{"org.opencontainers.image.revision": revision}}})
	man, _ := json.Marshal(map[string]any{"mediaType": "application/vnd.oci.image.manifest.v1+json", "config": map[string]string{"digest": digestOf(cfg)}})
	att, _ := json.Marshal(map[string]any{"mediaType": "application/vnd.oci.image.manifest.v1+json", "config": map[string]string{"digest": digestOf([]byte("x"))}})
	idx, _ := json.Marshal(map[string]any{"mediaType": "application/vnd.oci.image.index.v1+json", "manifests": []map[string]any{
		{"digest": digestOf(att), "platform": map[string]string{"os": "unknown", "architecture": "unknown"}},
		{"digest": digestOf(man), "platform": map[string]string{"os": "linux", "architecture": "amd64"}},
	}})
	blobs := map[string][]byte{digestOf(cfg): cfg, digestOf(man): man, digestOf(att): att, digestOf(idx): idx}
	if tamper {
		blobs[digestOf(cfg)] = []byte(`{"config":{"Labels":{"org.opencontainers.image.revision":"` + cNew + `"}}}`)
	}
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case r.URL.Path == "/token":
			if r.URL.Query().Get("scope") != "repository:nemecec/ruum-server:pull" {
				http.Error(w, "scope", 400)
				return
			}
			_, _ = w.Write([]byte(`{"token":"t0k"}`))
			return
		case r.Header.Get("Authorization") != "Bearer t0k":
			http.Error(w, "auth", 401)
			return
		case r.URL.Path == "/v2/nemecec/ruum-server/manifests/"+cNew && r.Method == http.MethodHead:
			w.Header().Set("Docker-Content-Digest", digestOf(idx))
			return
		}
		for _, kind := range []string{"manifests", "blobs"} {
			if d, ok := strings.CutPrefix(r.URL.Path, "/v2/nemecec/ruum-server/"+kind+"/"); ok {
				if b, ok := blobs[d]; ok {
					_, _ = w.Write(b)
					return
				}
			}
		}
		http.NotFound(w, r)
	}))
	t.Cleanup(srv.Close)
	return srv, digestOf(idx)
}

func TestGHCRTagAndRevision(t *testing.T) {
	srv, d := fakeGHCR(t, cNew, false)
	g := GHCR{Base: srv.URL, HTTP: srv.Client()}
	got, err := g.TagDigest(context.Background(), "nemecec/ruum-server", cNew)
	if err != nil || got != d {
		t.Fatalf("TagDigest = %q, %v; want %q", got, err, d)
	}
	if _, err := g.TagDigest(context.Background(), "nemecec/ruum-server", cOld); err == nil {
		t.Fatal("a missing tag gave a digest")
	}
	rev, err := g.Revision(context.Background(), "ghcr.io/nemecec/ruum-server@"+d)
	if err != nil || rev != cNew {
		t.Fatalf("Revision = %q, %v", rev, err)
	}
}

func TestGHCRRefusesContentThatDoesNotMatchItsDigest(t *testing.T) {
	srv, d := fakeGHCR(t, cSide, true)
	g := GHCR{Base: srv.URL, HTTP: srv.Client()}
	if rev, err := g.Revision(context.Background(), "ghcr.io/nemecec/ruum-server@"+d); err == nil {
		t.Fatalf("a changed config blob was accepted (revision %q)", rev)
	}
}

func TestGitHubCompareAndContents(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("User-Agent") == "" {
			http.Error(w, "ua", 403)
			return
		}
		switch r.URL.Path {
		case "/repos/nemecec/ruum-server/compare/" + cLive + "..." + cNew:
			_, _ = w.Write([]byte(`{"status":"ahead","files":[{"filename":"a.rs"},{"filename":"migrations/0042_x.sql","previous_filename":"migrations/old.sql"}]}`))
		case "/repos/nemecec/ruum-server/compare/" + cSide + "...main":
			_, _ = w.Write([]byte(`{"status":"diverged","files":[]}`))
		case "/repos/nemecec/ruum-server/compare/" + cOld + "...main":
			_, _ = w.Write([]byte(`{"status":"weird"}`))
		case "/repos/nemecec/ruum-server/contents/crates/ruum-server/src/migrate.rs":
			if r.URL.Query().Get("ref") == cNew {
				_, _ = w.Write([]byte(`{"type":"file"}`))
				return
			}
			http.NotFound(w, r)
		default:
			http.Error(w, "boom", 500)
		}
	}))
	defer srv.Close()
	g := GitHub{Base: srv.URL, Repo: "nemecec/ruum-server", HTTP: srv.Client()}
	ctx := context.Background()
	c, err := g.Compare(ctx, cLive, cNew)
	if err != nil || c.Status != "ahead" || len(c.Files) != 3 || c.Truncated {
		t.Fatalf("compare = %+v, %v", c, err)
	}
	if c, err := g.Compare(ctx, cSide, "main"); err != nil || c.Status != "diverged" {
		t.Fatalf("compare side = %+v, %v", c, err)
	}
	if _, err := g.Compare(ctx, cOld, "main"); err == nil {
		t.Fatal("an unknown status was accepted")
	}
	if ok, err := g.HasFile(ctx, cNew, "crates/ruum-server/src/migrate.rs"); !ok || err != nil {
		t.Fatalf("HasFile(new) = %v, %v", ok, err)
	}
	if ok, err := g.HasFile(ctx, cPre, "crates/ruum-server/src/migrate.rs"); ok || err != nil {
		t.Fatalf("HasFile(pre) = %v, %v", ok, err)
	}
	if _, err := g.HasFile(ctx, cNew, "other"); err == nil {
		t.Fatal("a server error was taken as an answer")
	}
}

// writeScript writes an executable shell script.
func writeScript(t *testing.T, body string) string {
	t.Helper()
	p := filepath.Join(t.TempDir(), "script")
	if err := os.WriteFile(p, []byte("#!/bin/sh\n"+body), 0o755); err != nil {
		t.Fatal(err)
	}
	return p
}

func TestCosignArgumentsAndRefusal(t *testing.T) {
	dir := t.TempDir()
	args := filepath.Join(dir, "args")
	ok := writeScript(t, `printf '%s\n' "$@" >`+args+"\nexit 0\n")
	ref := srv(dSrv)
	if err := (Cosign{Path: ok}).Verify(context.Background(), ref); err != nil {
		t.Fatal(err)
	}
	got, _ := os.ReadFile(args)
	want := strings.Join([]string{"verify-attestation", ref, "--type", provenanceType,
		"--certificate-oidc-issuer", provenanceIssuer, "--certificate-identity", provenanceIdentity}, "\n") + "\n"
	if string(got) != want {
		t.Fatalf("cosign args:\n%s\nwant:\n%s", got, want)
	}
	if !strings.HasSuffix(provenanceIdentity, "/.github/workflows/images.yml@refs/heads/main") {
		t.Fatal("the signer is not images.yml on refs/heads/main")
	}
	unsigned := writeScript(t, "echo 'Error: no matching attestations' >&2\nexit 1\n")
	if err := (Cosign{Path: unsigned}).Verify(context.Background(), ref); err == nil || !strings.Contains(err.Error(), "no matching attestations") {
		t.Fatalf("an unsigned image passed: %v", err)
	}
	if err := (Cosign{Path: ok}).Verify(context.Background(), "docker.io/library/busybox@"+dSrv); err == nil {
		t.Fatal("cosign was asked about a foreign image")
	}
}

func TestScriptRunStreamsLinesAndExitStatus(t *testing.T) {
	p := writeScript(t, "echo \"in-cluster=$RUUM_DEPLOY_IN_CLUSTER args=$*\"\necho err >&2\nexit 3\n")
	var lines []string
	err := Script{Path: p, Env: []string{"RUUM_DEPLOY_IN_CLUSTER=1"}}.Run(context.Background(), []string{"a", "b"}, func(l string) { lines = append(lines, l) })
	if err == nil {
		t.Fatal("exit 3 was not an error")
	}
	if strings.Join(lines, "|") != "in-cluster=1 args=a b|err" {
		t.Fatalf("lines %q", lines)
	}
}

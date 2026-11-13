package main

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"time"
)

// The signer of the images: images.yml of this repository on refs/heads/main, through GitHub's
// OIDC issuer (Sigstore public good). The same as cosign_verify_provenance in ops/image-check.sh.
const (
	provenanceType     = "https://slsa.dev/provenance/v1"
	provenanceIssuer   = "https://token.actions.githubusercontent.com"
	provenanceIdentity = "https://github.com/nemecec/ruum-server/.github/workflows/images.yml@refs/heads/main"
)

// Cosign verifies the SLSA provenance attestation with the cosign binary of the image.
type Cosign struct{ Path string }

// Verify implements Provenance.
func (c Cosign) Verify(ctx context.Context, ref string) error {
	if !serverRE.MatchString(ref) && !edgeRE.MatchString(ref) {
		return fmt.Errorf("not a Ruum image by digest: %s", ref)
	}
	cmd := exec.CommandContext(ctx, c.Path, "verify-attestation", ref,
		"--type", provenanceType,
		"--certificate-oidc-issuer", provenanceIssuer,
		"--certificate-identity", provenanceIdentity)
	out, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("cosign: %v: %s", err, lastLine(string(out)))
	}
	return nil
}

func lastLine(s string) string {
	s = strings.TrimSpace(s)
	if i := strings.LastIndexByte(s, '\n'); i >= 0 {
		s = s[i+1:]
	}
	if len(s) > 300 {
		s = s[:300]
	}
	return s
}

// Script runs ops/deploy-chart.sh (the deploy lock, the live release check, the backup, the
// migration Job and helm upgrade) in this pod, with the controller's ServiceAccount.
type Script struct {
	Path string
	Env  []string
}

// Run runs the script with args and calls line for every line of its output.
func (s Script) Run(ctx context.Context, args []string, line func(string)) error {
	cmd := exec.CommandContext(ctx, "bash", append([]string{s.Path}, args...)...)
	cmd.Env = append(os.Environ(), s.Env...)
	cmd.WaitDelay = 10 * time.Second
	r, w := io.Pipe()
	cmd.Stdout = w
	cmd.Stderr = w
	if err := cmd.Start(); err != nil {
		return err
	}
	done := make(chan struct{})
	go func() {
		sc := bufio.NewScanner(r)
		sc.Buffer(make([]byte, 64<<10), 1<<20)
		for sc.Scan() {
			line(sc.Text())
		}
		// Drain the rest if a line was too long.
		_, _ = io.Copy(io.Discard, r)
		close(done)
	}()
	err := cmd.Wait()
	w.Close()
	<-done
	return err
}

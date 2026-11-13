package main

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"regexp"
	"strings"
)

const manifestAccept = "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, " +
	"application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json"

var digestRE = regexp.MustCompile(`^sha256:[a-f0-9]{64}$`)

// GHCR reads public images of ghcr.io with an anonymous pull token, like ghcr_digest in
// ops/image-check.sh. Every manifest and blob it reads is checked against its digest.
type GHCR struct {
	Base string // https://ghcr.io
	HTTP *http.Client
}

func (g GHCR) token(ctx context.Context, image string) (string, error) {
	u := fmt.Sprintf("%s/token?scope=%s&service=ghcr.io", g.Base, url.QueryEscape("repository:"+image+":pull"))
	var out struct {
		Token string `json:"token"`
	}
	if err := getJSON(ctx, g.HTTP, u, nil, 1<<16, &out); err != nil {
		return "", err
	}
	if out.Token == "" {
		return "", fmt.Errorf("no pull token for %s", image)
	}
	return out.Token, nil
}

// TagDigest implements Registry.
func (g GHCR) TagDigest(ctx context.Context, image, tag string) (string, error) {
	tok, err := g.token(ctx, image)
	if err != nil {
		return "", err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodHead, fmt.Sprintf("%s/v2/%s/manifests/%s", g.Base, image, tag), nil)
	if err != nil {
		return "", err
	}
	req.Header.Set("Authorization", "Bearer "+tok)
	req.Header.Set("Accept", manifestAccept)
	resp, err := g.HTTP.Do(req)
	if err != nil {
		return "", err
	}
	resp.Body.Close()
	if resp.StatusCode == http.StatusNotFound {
		return "", fmt.Errorf("no tag %s", tag)
	}
	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("HEAD manifest: %s", resp.Status)
	}
	d := resp.Header.Get("Docker-Content-Digest")
	if !digestRE.MatchString(d) {
		return "", fmt.Errorf("unexpected digest %q", d)
	}
	return d, nil
}

// blob GETs a manifest or blob by digest and checks that its content has that digest.
func (g GHCR) blob(ctx context.Context, tok, image, kind, digest string) ([]byte, error) {
	if !digestRE.MatchString(digest) {
		return nil, fmt.Errorf("unexpected digest %q", digest)
	}
	h := http.Header{"Authorization": {"Bearer " + tok}}
	if kind == "manifests" {
		h.Set("Accept", manifestAccept)
	}
	body, err := get(ctx, g.HTTP, fmt.Sprintf("%s/v2/%s/%s/%s", g.Base, image, kind, digest), h, 4<<20)
	if err != nil {
		return nil, err
	}
	sum := sha256.Sum256(body)
	if "sha256:"+hex.EncodeToString(sum[:]) != digest {
		return nil, fmt.Errorf("the content of %s does not match its digest", digest)
	}
	return body, nil
}

// Revision implements Registry: the label org.opencontainers.image.revision of the linux/amd64
// image of ref (images.yml sets it to the commit it built).
func (g GHCR) Revision(ctx context.Context, ref string) (string, error) {
	image, digest, ok := strings.Cut(strings.TrimPrefix(ref, "ghcr.io/"), "@")
	if !ok {
		return "", fmt.Errorf("not a reference by digest: %s", ref)
	}
	tok, err := g.token(ctx, image)
	if err != nil {
		return "", err
	}
	body, err := g.blob(ctx, tok, image, "manifests", digest)
	if err != nil {
		return "", err
	}
	var m struct {
		Manifests []struct {
			Digest   string `json:"digest"`
			Platform *struct {
				OS           string `json:"os"`
				Architecture string `json:"architecture"`
			} `json:"platform"`
		} `json:"manifests"`
		Config *struct {
			Digest string `json:"digest"`
		} `json:"config"`
	}
	if err := json.Unmarshal(body, &m); err != nil {
		return "", err
	}
	if m.Config == nil {
		// An index: the linux/amd64 image (the other entries are attestations).
		found := ""
		for _, e := range m.Manifests {
			if e.Platform != nil && e.Platform.OS == "linux" && e.Platform.Architecture == "amd64" {
				found = e.Digest
				break
			}
		}
		if found == "" {
			return "", fmt.Errorf("%s has no linux/amd64 image", ref)
		}
		if body, err = g.blob(ctx, tok, image, "manifests", found); err != nil {
			return "", err
		}
		m.Config = nil
		if err := json.Unmarshal(body, &m); err != nil {
			return "", err
		}
		if m.Config == nil {
			return "", fmt.Errorf("the linux/amd64 manifest of %s has no config", ref)
		}
	}
	body, err = g.blob(ctx, tok, image, "blobs", m.Config.Digest)
	if err != nil {
		return "", err
	}
	var cfg struct {
		Config struct {
			Labels map[string]string `json:"Labels"`
		} `json:"config"`
	}
	if err := json.Unmarshal(body, &cfg); err != nil {
		return "", err
	}
	return cfg.Config.Labels["org.opencontainers.image.revision"], nil
}

// get reads at most limit bytes of a 200 answer.
func get(ctx context.Context, c *http.Client, u string, h http.Header, limit int64) ([]byte, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u, nil)
	if err != nil {
		return nil, err
	}
	for k, v := range h {
		req.Header[k] = v
	}
	resp, err := c.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, &httpError{URL: u, Status: resp.StatusCode}
	}
	body, err := io.ReadAll(io.LimitReader(resp.Body, limit+1))
	if err != nil {
		return nil, err
	}
	if int64(len(body)) > limit {
		return nil, fmt.Errorf("the answer of %s is larger than %d bytes", u, limit)
	}
	return body, nil
}

func getJSON(ctx context.Context, c *http.Client, u string, h http.Header, limit int64, out any) error {
	body, err := get(ctx, c, u, h, limit)
	if err != nil {
		return err
	}
	return json.Unmarshal(body, out)
}

type httpError struct {
	URL    string
	Status int
}

func (e *httpError) Error() string { return fmt.Sprintf("GET %s: HTTP %d", e.URL, e.Status) }

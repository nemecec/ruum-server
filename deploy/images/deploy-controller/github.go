package main

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"strings"
)

// GitHub reads the history of the public repository through GitHub's REST API, without a token
// (60 requests per hour per address; a deploy needs at most five).
type GitHub struct {
	Base string // https://api.github.com
	Repo string // nemecec/ruum-server
	HTTP *http.Client
}

func (g GitHub) headers() http.Header {
	return http.Header{
		"Accept":               {"application/vnd.github+json"},
		"X-Github-Api-Version": {"2022-11-28"},
		"User-Agent":           {"ruum-deploy-controller"},
	}
}

// githubFileLimit: the compare API lists at most 300 files.
const githubFileLimit = 300

// Compare implements Git (GET /repos/{repo}/compare/{base}...{head}).
func (g GitHub) Compare(ctx context.Context, base, head string) (Comparison, error) {
	var out struct {
		Status string `json:"status"`
		Files  []struct {
			Filename         string `json:"filename"`
			PreviousFilename string `json:"previous_filename"`
		} `json:"files"`
	}
	u := fmt.Sprintf("%s/repos/%s/compare/%s...%s", g.Base, g.Repo, url.PathEscape(base), url.PathEscape(head))
	if err := getJSON(ctx, g.HTTP, u, g.headers(), 64<<20, &out); err != nil {
		return Comparison{}, err
	}
	switch out.Status {
	case "ahead", "behind", "identical", "diverged":
	default:
		return Comparison{}, fmt.Errorf("unexpected compare status %q", out.Status)
	}
	c := Comparison{Status: out.Status, Truncated: len(out.Files) >= githubFileLimit}
	for _, f := range out.Files {
		c.Files = append(c.Files, f.Filename)
		if f.PreviousFilename != "" {
			c.Files = append(c.Files, f.PreviousFilename)
		}
	}
	return c, nil
}

// HasFile implements Git (GET /repos/{repo}/contents/{path}?ref={commit}).
func (g GitHub) HasFile(ctx context.Context, commit, path string) (bool, error) {
	parts := strings.Split(path, "/")
	for i, p := range parts {
		parts[i] = url.PathEscape(p)
	}
	u := fmt.Sprintf("%s/repos/%s/contents/%s?ref=%s", g.Base, g.Repo, strings.Join(parts, "/"), url.QueryEscape(commit))
	_, err := get(ctx, g.HTTP, u, g.headers(), 16<<20)
	var he *httpError
	if errors.As(err, &he) && he.Status == http.StatusNotFound {
		return false, nil
	}
	return err == nil, err
}

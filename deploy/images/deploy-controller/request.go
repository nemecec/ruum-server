package main

import (
	"fmt"
	"regexp"
	"sort"
	"strings"
)

// The patterns are the same as deploy/staging/ops/image-check.sh (code review F-3).
var (
	commitRE = regexp.MustCompile(`^[0-9a-f]{40}$`)
	serverRE = regexp.MustCompile(`^ghcr\.io/nemecec/ruum-server@sha256:[a-f0-9]{64}$`)
	edgeRE   = regexp.MustCompile(`^ghcr\.io/nemecec/ruum-edge@sha256:[a-f0-9]{64}$`)
	emailRE  = regexp.MustCompile(`^[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9-]{1,63}(\.[A-Za-z0-9-]{1,63})*\.[A-Za-z]{2,24}$`)
)

// acmeIssuers are the ACME issuers of the chart: never production Let's Encrypt (review 2 R-3).
var acmeIssuers = map[string]bool{"letsencrypt-staging": true, "zerossl": true}

// The rollback modes of a request (deploy.sh --rollback, --rollback-older).
const (
	RollbackNone     = "none"
	RollbackPrevious = "previous"
	RollbackOlder    = "older"
)

// Request is a release request: the data of a ConfigMap with the label
// ruum.space/release-request=true in the controller's namespace, written by the deployer
// (ops/release-request.sh). Every field is checked here; nothing else in the ConfigMap is used.
type Request struct {
	Commit         string // the commit of main, 40 hex
	Server         string // ghcr.io/nemecec/ruum-server@sha256:...
	Edge           string // ghcr.io/nemecec/ruum-edge@sha256:...
	ExpectDeployed string // the live commit that deploy.sh read, or "none"
	Rollback       string // none, previous or older
	Acme           string // optional: letsencrypt-staging or zerossl
	AcmeEmail      string // optional: the ACME account email (zerossl)
}

// requestKeys are the only data keys a new request may have.
var requestKeys = map[string]bool{
	"commit": true, "server": true, "edge": true, "expectDeployed": true,
	"rollback": true, "acme": true, "acmeEmail": true,
}

// Refusal is a request that the controller does not run, with the reason for the person.
type Refusal struct{ Reason string }

func (r *Refusal) Error() string { return r.Reason }

func refuse(format string, args ...any) error { return &Refusal{Reason: fmt.Sprintf(format, args...)} }

// ParseRequest checks the data of a request ConfigMap.
func ParseRequest(data map[string]string) (Request, error) {
	var unknown []string
	for k := range data {
		if !requestKeys[k] {
			unknown = append(unknown, k)
		}
	}
	if len(unknown) > 0 {
		sort.Strings(unknown)
		return Request{}, refuse("unknown keys in the request: %s", strings.Join(unknown, ", "))
	}
	r := Request{
		Commit:         data["commit"],
		Server:         data["server"],
		Edge:           data["edge"],
		ExpectDeployed: data["expectDeployed"],
		Rollback:       data["rollback"],
		Acme:           data["acme"],
		AcmeEmail:      data["acmeEmail"],
	}
	if r.Rollback == "" {
		r.Rollback = RollbackNone
	}
	if !commitRE.MatchString(r.Commit) {
		return Request{}, refuse("the commit must be 40 lowercase hex characters")
	}
	if !serverRE.MatchString(r.Server) {
		return Request{}, refuse("the server image must be ghcr.io/nemecec/ruum-server@sha256:<digest>")
	}
	if !edgeRE.MatchString(r.Edge) {
		return Request{}, refuse("the edge image must be ghcr.io/nemecec/ruum-edge@sha256:<digest>")
	}
	if r.ExpectDeployed != "none" && !commitRE.MatchString(r.ExpectDeployed) {
		return Request{}, refuse("expectDeployed must be the live commit that deploy.sh read (40 lowercase hex characters) or none")
	}
	switch r.Rollback {
	case RollbackNone, RollbackPrevious, RollbackOlder:
	default:
		return Request{}, refuse("rollback must be none, previous or older")
	}
	if r.Rollback != RollbackNone && r.ExpectDeployed == "none" {
		return Request{}, refuse("a rollback needs a live release (expectDeployed)")
	}
	if r.Acme != "" && !acmeIssuers[r.Acme] {
		return Request{}, refuse("acme must be letsencrypt-staging or zerossl")
	}
	if r.AcmeEmail != "" && !emailRE.MatchString(r.AcmeEmail) {
		return Request{}, refuse("acmeEmail is not a plain email address")
	}
	if r.Acme == "zerossl" && r.AcmeEmail == "" {
		return Request{}, refuse("acme zerossl needs acmeEmail")
	}
	return r, nil
}

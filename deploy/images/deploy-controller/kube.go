package main

import (
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"
)

// ConfigMap is the part of a ConfigMap that the controller reads and writes.
type ConfigMap struct {
	APIVersion string            `json:"apiVersion"`
	Kind       string            `json:"kind"`
	Metadata   ObjectMeta        `json:"metadata"`
	Data       map[string]string `json:"data,omitempty"`
}

// ObjectMeta is the metadata of a ConfigMap.
type ObjectMeta struct {
	Name              string            `json:"name"`
	Namespace         string            `json:"namespace,omitempty"`
	UID               string            `json:"uid,omitempty"`
	ResourceVersion   string            `json:"resourceVersion,omitempty"`
	CreationTimestamp time.Time         `json:"creationTimestamp,omitzero"`
	Labels            map[string]string `json:"labels,omitempty"`
	Annotations       map[string]string `json:"annotations,omitempty"`
}

// ErrConflict is a write with an old resourceVersion (someone else changed the object).
var ErrConflict = errors.New("conflict: the object has changed")

// ErrNotFound is a missing object.
var ErrNotFound = errors.New("not found")

// Store keeps the release requests (ConfigMaps of one namespace).
type Store interface {
	List(ctx context.Context) ([]ConfigMap, error)
	Get(ctx context.Context, name string) (ConfigMap, error)
	Update(ctx context.Context, cm ConfigMap) (ConfigMap, error)
	Delete(ctx context.Context, name string) error
}

const saDir = "/var/run/secrets/kubernetes.io/serviceaccount"

// Kube is a small client of the Kubernetes API with the pod's ServiceAccount (standard library
// only). The token is read for every call, because projected tokens rotate.
type Kube struct {
	Base      string
	TokenFile string
	HTTP      *http.Client
}

// InCluster builds the client from the pod's environment.
func InCluster() (*Kube, error) {
	host, port := os.Getenv("KUBERNETES_SERVICE_HOST"), os.Getenv("KUBERNETES_SERVICE_PORT")
	if host == "" || port == "" {
		return nil, errors.New("not in a Kubernetes pod (KUBERNETES_SERVICE_HOST is not set)")
	}
	ca, err := os.ReadFile(saDir + "/ca.crt")
	if err != nil {
		return nil, err
	}
	pool := x509.NewCertPool()
	if !pool.AppendCertsFromPEM(ca) {
		return nil, errors.New("no CA certificate in the ServiceAccount mount")
	}
	tr := &http.Transport{TLSClientConfig: &tls.Config{RootCAs: pool, MinVersion: tls.VersionTLS12}}
	return &Kube{
		Base:      "https://" + net.JoinHostPort(host, port),
		TokenFile: saDir + "/token",
		HTTP:      &http.Client{Transport: tr, Timeout: 30 * time.Second},
	}, nil
}

func (k *Kube) do(ctx context.Context, method, path string, body any, out any) error {
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, k.Base+path, rd)
	if err != nil {
		return err
	}
	if k.TokenFile != "" {
		tok, err := os.ReadFile(k.TokenFile)
		if err != nil {
			return err
		}
		req.Header.Set("Authorization", "Bearer "+strings.TrimSpace(string(tok)))
	}
	req.Header.Set("Accept", "application/json")
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	resp, err := k.HTTP.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(io.LimitReader(resp.Body, 32<<20))
	if err != nil {
		return err
	}
	switch {
	case resp.StatusCode == http.StatusConflict:
		return ErrConflict
	case resp.StatusCode == http.StatusNotFound:
		return ErrNotFound
	case resp.StatusCode/100 != 2:
		return fmt.Errorf("%s %s: HTTP %d: %s", method, path, resp.StatusCode, lastLine(string(data)))
	}
	if out != nil {
		return json.Unmarshal(data, out)
	}
	return nil
}

// Requests is the Store of release requests in one namespace.
type Requests struct {
	Kube      *Kube
	Namespace string
}

func (r Requests) path(name string) string {
	p := "/api/v1/namespaces/" + url.PathEscape(r.Namespace) + "/configmaps"
	if name != "" {
		p += "/" + url.PathEscape(name)
	}
	return p
}

// List implements Store: the ConfigMaps with the label ruum.space/release-request=true.
func (r Requests) List(ctx context.Context) ([]ConfigMap, error) {
	var out struct {
		Items []ConfigMap `json:"items"`
	}
	sel := url.QueryEscape(requestLabel + "=true")
	if err := r.Kube.do(ctx, http.MethodGet, r.path("")+"?labelSelector="+sel, nil, &out); err != nil {
		return nil, err
	}
	return out.Items, nil
}

// Get implements Store.
func (r Requests) Get(ctx context.Context, name string) (ConfigMap, error) {
	var cm ConfigMap
	err := r.Kube.do(ctx, http.MethodGet, r.path(name), nil, &cm)
	return cm, err
}

// Update implements Store: a PUT with the resourceVersion that was read (compare and swap).
func (r Requests) Update(ctx context.Context, cm ConfigMap) (ConfigMap, error) {
	cm.APIVersion, cm.Kind = "v1", "ConfigMap"
	var out ConfigMap
	err := r.Kube.do(ctx, http.MethodPut, r.path(cm.Metadata.Name), cm, &out)
	return out, err
}

// Delete implements Store.
func (r Requests) Delete(ctx context.Context, name string) error {
	return r.Kube.do(ctx, http.MethodDelete, r.path(name), nil, nil)
}

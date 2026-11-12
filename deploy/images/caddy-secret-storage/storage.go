// Package secretstorage is a Caddy storage module that keeps certificates, ACME accounts and
// challenge tokens in Kubernetes Secrets of one namespace, and locks with Leases there
// (Ruum backlog O-26, design 12.7). Every edge pod of the namespace shares them: a restart
// loads the certificates instead of issuing new ones, and any pod answers an HTTP-01 challenge
// that another pod started.
//
// The idea follows the secret store of github.com/caddyserver/ingress; this is a separate,
// smaller implementation:
//   - a Secret per key, named caddy-<40 hex of SHA-256 of the key>, so any key fits the name
//     rules and two keys never share a name; the key itself is in an annotation;
//   - errors are returned, never dropped; a missing key is fs.ErrNotExist, as certmagic wants;
//   - a lock is a Lease with a holder token per Lock call, renewed while held, taken over only
//     when it expired, with optimistic concurrency (resourceVersion) on every update.
//
// Caddyfile:
//
//	storage kubernetes_secret {
//		namespace edge
//	}
//
// The pod needs a ServiceAccount token with get, list, create, update and delete on Secrets,
// and get, create, update and delete on Leases, in that namespace only.
package secretstorage

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"regexp"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/caddyserver/caddy/v2"
	"github.com/caddyserver/caddy/v2/caddyconfig/caddyfile"
	"github.com/caddyserver/certmagic"
	"go.uber.org/zap"
	coordinationv1 "k8s.io/api/coordination/v1"
	corev1 "k8s.io/api/core/v1"
	apierrors "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes"
	"k8s.io/client-go/rest"
	"k8s.io/client-go/util/retry"
)

const (
	// managedLabel marks the Secrets and Leases of this module.
	managedLabel = "app.kubernetes.io/managed-by"
	managedValue = "caddy-secret-storage"
	// keyAnnotation holds the certmagic key of a Secret or the lock name of a Lease.
	keyAnnotation = "ruum.space/caddy-key"
	// modifiedAnnotation holds the time of the last Store (RFC 3339, UTC).
	modifiedAnnotation = "ruum.space/caddy-modified"
	dataKey            = "value"
	namespaceFile      = "/var/run/secrets/kubernetes.io/serviceaccount/namespace"
)

// Lock timing. Variables, so the tests can shorten them.
var (
	leaseDuration = 60 * time.Second
	renewEvery    = 20 * time.Second
	lockPoll      = 2 * time.Second
)

var dnsLabel = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]{0,61}[a-z0-9])?$`)

func init() {
	caddy.RegisterModule(new(Storage))
}

// Storage is the module caddy.storage.kubernetes_secret.
type Storage struct {
	// Namespace of the Secrets and Leases. Default: the pod's own namespace.
	Namespace string `json:"namespace,omitempty"`

	client kubernetes.Interface
	holder string
	logger *zap.Logger

	mu    sync.Mutex
	locks map[string]*heldLock
}

type heldLock struct {
	lease string
	token string
	stop  context.CancelFunc
	done  chan struct{}
}

// CaddyModule returns the module information.
func (*Storage) CaddyModule() caddy.ModuleInfo {
	return caddy.ModuleInfo{
		ID:  "caddy.storage.kubernetes_secret",
		New: func() caddy.Module { return new(Storage) },
	}
}

// Provision connects to the Kubernetes API with the pod's ServiceAccount.
func (s *Storage) Provision(ctx caddy.Context) error {
	s.logger = ctx.Logger(s)
	if s.client == nil {
		config, err := rest.InClusterConfig()
		if err != nil {
			return fmt.Errorf("kubernetes_secret storage: no in-cluster config: %w", err)
		}
		client, err := kubernetes.NewForConfig(config)
		if err != nil {
			return fmt.Errorf("kubernetes_secret storage: client: %w", err)
		}
		s.client = client
	}
	return s.setup()
}

// setup fills the defaults that need no API call.
func (s *Storage) setup() error {
	if s.logger == nil {
		s.logger = zap.NewNop()
	}
	if s.Namespace == "" {
		ns, err := os.ReadFile(namespaceFile)
		if err != nil {
			return fmt.Errorf("kubernetes_secret storage: no namespace set and none in %s: %w", namespaceFile, err)
		}
		s.Namespace = strings.TrimSpace(string(ns))
	}
	if !dnsLabel.MatchString(s.Namespace) {
		return fmt.Errorf("kubernetes_secret storage: %q is not a namespace name", s.Namespace)
	}
	if s.holder == "" {
		s.holder = os.Getenv("POD_NAME")
	}
	if s.holder == "" {
		s.holder, _ = os.Hostname()
	}
	if s.locks == nil {
		s.locks = map[string]*heldLock{}
	}
	return nil
}

// CertMagicStorage returns the storage for certmagic.
func (s *Storage) CertMagicStorage() (certmagic.Storage, error) {
	return s, nil
}

// UnmarshalCaddyfile reads the Caddyfile block.
func (s *Storage) UnmarshalCaddyfile(d *caddyfile.Dispenser) error {
	d.Next() // the module name
	if d.NextArg() {
		return d.ArgErr()
	}
	for d.NextBlock(0) {
		switch d.Val() {
		case "namespace":
			if !d.NextArg() {
				return d.ArgErr()
			}
			s.Namespace = d.Val()
			if d.NextArg() {
				return d.ArgErr()
			}
		default:
			return d.Errf("unknown option %q", d.Val())
		}
	}
	return nil
}

func cleanKey(key string) string {
	return strings.Trim(key, "/")
}

func hashName(prefix, key string) string {
	sum := sha256.Sum256([]byte(key))
	return prefix + hex.EncodeToString(sum[:])[:40]
}

func secretName(key string) string { return hashName("caddy-", key) }
func leaseName(lock string) string { return hashName("caddy-lock-", lock) }

// Store writes value at key, creating or replacing it.
func (s *Storage) Store(ctx context.Context, key string, value []byte) error {
	key = cleanKey(key)
	now := time.Now().UTC().Format(time.RFC3339Nano)
	secret := &corev1.Secret{
		ObjectMeta: metav1.ObjectMeta{
			Name:        secretName(key),
			Namespace:   s.Namespace,
			Labels:      map[string]string{managedLabel: managedValue},
			Annotations: map[string]string{keyAnnotation: key, modifiedAnnotation: now},
		},
		Type: corev1.SecretTypeOpaque,
		Data: map[string][]byte{dataKey: value},
	}
	secrets := s.client.CoreV1().Secrets(s.Namespace)
	_, err := secrets.Create(ctx, secret, metav1.CreateOptions{})
	if err == nil || !apierrors.IsAlreadyExists(err) {
		return err
	}
	return retry.RetryOnConflict(retry.DefaultRetry, func() error {
		current, err := secrets.Get(ctx, secret.Name, metav1.GetOptions{})
		if err != nil {
			return err
		}
		if current.Annotations == nil {
			current.Annotations = map[string]string{}
		}
		current.Annotations[keyAnnotation] = key
		current.Annotations[modifiedAnnotation] = now
		current.Data = map[string][]byte{dataKey: value}
		_, err = secrets.Update(ctx, current, metav1.UpdateOptions{})
		return err
	})
}

// get returns the Secret of key, or fs.ErrNotExist.
func (s *Storage) get(ctx context.Context, key string) (*corev1.Secret, error) {
	secret, err := s.client.CoreV1().Secrets(s.Namespace).Get(ctx, secretName(key), metav1.GetOptions{})
	if apierrors.IsNotFound(err) {
		return nil, fs.ErrNotExist
	}
	if err != nil {
		return nil, err
	}
	if secret.Annotations[keyAnnotation] != key {
		return nil, fs.ErrNotExist
	}
	return secret, nil
}

// Load returns the value at key.
func (s *Storage) Load(ctx context.Context, key string) ([]byte, error) {
	secret, err := s.get(ctx, cleanKey(key))
	if err != nil {
		return nil, err
	}
	return secret.Data[dataKey], nil
}

// Exists says whether key holds a value.
func (s *Storage) Exists(ctx context.Context, key string) bool {
	_, err := s.get(ctx, cleanKey(key))
	return err == nil
}

// keys lists the keys of every Secret of this module, page by page.
func (s *Storage) keys(ctx context.Context) ([]string, error) {
	var keys []string
	opts := metav1.ListOptions{LabelSelector: managedLabel + "=" + managedValue, Limit: 500}
	for {
		page, err := s.client.CoreV1().Secrets(s.Namespace).List(ctx, opts)
		if err != nil {
			return nil, err
		}
		for _, item := range page.Items {
			if k, ok := item.Annotations[keyAnnotation]; ok && item.Name == secretName(k) {
				keys = append(keys, k)
			}
		}
		if page.Continue == "" {
			return keys, nil
		}
		opts.Continue = page.Continue
	}
}

// under returns the keys below prefix ("" is the root).
func under(keys []string, prefix string) []string {
	var out []string
	for _, k := range keys {
		if prefix == "" || strings.HasPrefix(k, prefix+"/") {
			out = append(out, k)
		}
	}
	return out
}

// errDeleteRoot is the answer of Delete for the root ("", "/"): one call must never remove all
// the TLS state (certificates, ACME accounts) of the edge. List and Stat accept the root,
// because certmagic lists from there.
var errDeleteRoot = errors.New("refusing to delete the storage root")

// Delete removes key and, when key is a directory, every key below it. A key that is already
// gone is not an error. The root is refused (errDeleteRoot).
func (s *Storage) Delete(ctx context.Context, key string) error {
	key = cleanKey(key)
	if key == "" {
		return errDeleteRoot
	}
	secrets := s.client.CoreV1().Secrets(s.Namespace)
	names := []string{secretName(key)}
	all, err := s.keys(ctx)
	if err != nil {
		return err
	}
	for _, k := range under(all, key) {
		names = append(names, secretName(k))
	}
	var errs []error
	for _, name := range names {
		if err := secrets.Delete(ctx, name, metav1.DeleteOptions{}); err != nil && !apierrors.IsNotFound(err) {
			errs = append(errs, err)
		}
	}
	return errors.Join(errs...)
}

// List returns the keys below prefix: all of them when recursive, else only the direct children
// (files and directories). fs.ErrNotExist when there is none.
func (s *Storage) List(ctx context.Context, prefix string, recursive bool) ([]string, error) {
	prefix = cleanKey(prefix)
	all, err := s.keys(ctx)
	if err != nil {
		return nil, err
	}
	seen := map[string]bool{}
	var out []string
	for _, k := range under(all, prefix) {
		entry := k
		if !recursive {
			rest := strings.TrimPrefix(k, prefix)
			rest = strings.TrimPrefix(rest, "/")
			first, _, _ := strings.Cut(rest, "/")
			entry = first
			if prefix != "" {
				entry = prefix + "/" + first
			}
		}
		if !seen[entry] {
			seen[entry] = true
			out = append(out, entry)
		}
	}
	if len(out) == 0 {
		return nil, fs.ErrNotExist
	}
	sort.Strings(out)
	return out, nil
}

// Stat describes key: a value (terminal), or a directory that has keys below it.
func (s *Storage) Stat(ctx context.Context, key string) (certmagic.KeyInfo, error) {
	key = cleanKey(key)
	secret, err := s.get(ctx, key)
	if err == nil {
		modified := secret.CreationTimestamp.UTC()
		if t, perr := time.Parse(time.RFC3339Nano, secret.Annotations[modifiedAnnotation]); perr == nil {
			modified = t
		}
		return certmagic.KeyInfo{
			Key:        key,
			Modified:   modified,
			Size:       int64(len(secret.Data[dataKey])),
			IsTerminal: true,
		}, nil
	}
	if !errors.Is(err, fs.ErrNotExist) {
		return certmagic.KeyInfo{}, err
	}
	all, err := s.keys(ctx)
	if err != nil {
		return certmagic.KeyInfo{}, err
	}
	if len(under(all, key)) > 0 {
		return certmagic.KeyInfo{Key: key, IsTerminal: false}, nil
	}
	return certmagic.KeyInfo{}, fs.ErrNotExist
}

func randomToken() string {
	b := make([]byte, 8)
	_, _ = rand.Read(b)
	return hex.EncodeToString(b)
}

func expired(lease *coordinationv1.Lease, now time.Time) bool {
	spec := lease.Spec
	if spec.HolderIdentity == nil || *spec.HolderIdentity == "" || spec.RenewTime == nil {
		return true
	}
	duration := leaseDuration
	if spec.LeaseDurationSeconds != nil {
		duration = time.Duration(*spec.LeaseDurationSeconds) * time.Second
	}
	return spec.RenewTime.Add(duration).Before(now)
}

func (s *Storage) leaseSpec(token string, now metav1.MicroTime) coordinationv1.LeaseSpec {
	seconds := int32(leaseDuration / time.Second)
	if seconds < 1 {
		seconds = 1
	}
	return coordinationv1.LeaseSpec{
		HolderIdentity:       &token,
		LeaseDurationSeconds: &seconds,
		AcquireTime:          &now,
		RenewTime:            &now,
	}
}

// tryAcquire takes the Lease when it is free or expired. false (without an error) when another
// holder has it.
func (s *Storage) tryAcquire(ctx context.Context, lock, name, token string) (bool, error) {
	leases := s.client.CoordinationV1().Leases(s.Namespace)
	now := metav1.NowMicro()
	lease := &coordinationv1.Lease{
		ObjectMeta: metav1.ObjectMeta{
			Name:        name,
			Namespace:   s.Namespace,
			Labels:      map[string]string{managedLabel: managedValue},
			Annotations: map[string]string{keyAnnotation: lock},
		},
		Spec: s.leaseSpec(token, now),
	}
	_, err := leases.Create(ctx, lease, metav1.CreateOptions{})
	if err == nil {
		return true, nil
	}
	if !apierrors.IsAlreadyExists(err) {
		return false, err
	}
	current, err := leases.Get(ctx, name, metav1.GetOptions{})
	if apierrors.IsNotFound(err) {
		return false, nil // released in between; try again
	}
	if err != nil {
		return false, err
	}
	if !expired(current, now.Time) {
		return false, nil
	}
	// Take over an expired lease. The resourceVersion of the Get makes this fail with a conflict
	// when another pod took it first.
	current.Spec = s.leaseSpec(token, now)
	if _, err := leases.Update(ctx, current, metav1.UpdateOptions{}); err != nil {
		if apierrors.IsConflict(err) {
			return false, nil
		}
		return false, err
	}
	s.logger.Info("took over an expired storage lock", zap.String("lock", lock))
	return true, nil
}

// Lock blocks until it holds the lock name, or ctx ends.
func (s *Storage) Lock(ctx context.Context, name string) error {
	name = cleanKey(name)
	lease := leaseName(name)
	token := s.holder + "/" + randomToken()
	for {
		got, err := s.tryAcquire(ctx, name, lease, token)
		if err != nil {
			return fmt.Errorf("lock %q: %w", name, err)
		}
		if got {
			renewCtx, stop := context.WithCancel(context.Background())
			held := &heldLock{lease: lease, token: token, stop: stop, done: make(chan struct{})}
			go s.renew(renewCtx, held)
			s.mu.Lock()
			s.locks[name] = held
			s.mu.Unlock()
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(lockPoll):
		}
	}
}

// renew keeps a held Lease fresh until Unlock, and stops when another holder took it.
func (s *Storage) renew(ctx context.Context, held *heldLock) {
	defer close(held.done)
	leases := s.client.CoordinationV1().Leases(s.Namespace)
	ticker := time.NewTicker(renewEvery)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
		err := retry.RetryOnConflict(retry.DefaultRetry, func() error {
			current, err := leases.Get(ctx, held.lease, metav1.GetOptions{})
			if err != nil {
				return err
			}
			if current.Spec.HolderIdentity == nil || *current.Spec.HolderIdentity != held.token {
				return errLost
			}
			now := metav1.NowMicro()
			current.Spec.RenewTime = &now
			_, err = leases.Update(ctx, current, metav1.UpdateOptions{})
			return err
		})
		if errors.Is(err, errLost) || apierrors.IsNotFound(err) {
			s.logger.Warn("a storage lock was lost", zap.String("lease", held.lease))
			return
		}
		if err != nil && ctx.Err() == nil {
			s.logger.Warn("renewing a storage lock failed", zap.String("lease", held.lease), zap.Error(err))
		}
	}
}

var errLost = errors.New("lock held by another holder")

// Unlock releases the lock name if this storage holds it.
func (s *Storage) Unlock(ctx context.Context, name string) error {
	name = cleanKey(name)
	s.mu.Lock()
	held, ok := s.locks[name]
	delete(s.locks, name)
	s.mu.Unlock()
	if !ok {
		return fmt.Errorf("unlock %q: not held here", name)
	}
	held.stop()
	<-held.done
	leases := s.client.CoordinationV1().Leases(s.Namespace)
	current, err := leases.Get(ctx, held.lease, metav1.GetOptions{})
	if apierrors.IsNotFound(err) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("unlock %q: %w", name, err)
	}
	if current.Spec.HolderIdentity == nil || *current.Spec.HolderIdentity != held.token {
		return nil // expired and taken over: not ours to delete
	}
	uid, version := current.UID, current.ResourceVersion
	err = leases.Delete(ctx, held.lease, metav1.DeleteOptions{
		Preconditions: &metav1.Preconditions{UID: &uid, ResourceVersion: &version},
	})
	if err != nil && !apierrors.IsNotFound(err) {
		return fmt.Errorf("unlock %q: %w", name, err)
	}
	return nil
}

// Interface guards.
var (
	_ caddy.Module           = (*Storage)(nil)
	_ caddy.Provisioner      = (*Storage)(nil)
	_ caddy.StorageConverter = (*Storage)(nil)
	_ caddyfile.Unmarshaler  = (*Storage)(nil)
	_ certmagic.Storage      = (*Storage)(nil)
)

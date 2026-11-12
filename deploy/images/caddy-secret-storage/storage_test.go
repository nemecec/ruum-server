package secretstorage

import (
	"context"
	"errors"
	"io/fs"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/caddyserver/caddy/v2/caddyconfig/caddyfile"
	coordinationv1 "k8s.io/api/coordination/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/client-go/kubernetes/fake"
)

func newStorage(t *testing.T) *Storage {
	t.Helper()
	leaseDuration, renewEvery, lockPoll = 2*time.Second, 200*time.Millisecond, 50*time.Millisecond
	s := &Storage{Namespace: "edge", client: fake.NewClientset(), holder: "edge-test"}
	if err := s.setup(); err != nil {
		t.Fatal(err)
	}
	return s
}

func TestStoreLoadExists(t *testing.T) {
	ctx := context.Background()
	s := newStorage(t)
	key := "certificates/acme.zerossl.com-v2-dv90/staging.ruum.space/staging.ruum.space.crt"
	if s.Exists(ctx, key) {
		t.Fatal("exists before Store")
	}
	if _, err := s.Load(ctx, key); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("Load of a missing key: %v, want fs.ErrNotExist", err)
	}
	if err := s.Store(ctx, key, []byte("one")); err != nil {
		t.Fatal(err)
	}
	if err := s.Store(ctx, key, []byte("two")); err != nil {
		t.Fatal("overwrite:", err)
	}
	got, err := s.Load(ctx, key)
	if err != nil || string(got) != "two" {
		t.Fatalf("Load = %q, %v", got, err)
	}
	if !s.Exists(ctx, "/"+key+"/") {
		t.Fatal("a key with slashes around it is the same key")
	}
	secret, err := s.client.CoreV1().Secrets("edge").Get(ctx, secretName(key), metav1.GetOptions{})
	if err != nil {
		t.Fatal(err)
	}
	if secret.Labels[managedLabel] != managedValue || secret.Annotations[keyAnnotation] != key {
		t.Fatalf("labels %v annotations %v", secret.Labels, secret.Annotations)
	}
}

func TestLongKeysFitTheNameRules(t *testing.T) {
	ctx := context.Background()
	s := newStorage(t)
	host := strings.Repeat("a", 63) + ".content.staging.ruumapps.net"
	key := "certificates/acme.zerossl.com-v2-dv90/" + host + "/" + host + ".key"
	if err := s.Store(ctx, key, []byte("k")); err != nil {
		t.Fatal(err)
	}
	if name := secretName(key); len(name) > 63 || !dnsLabel.MatchString(name) {
		t.Fatalf("secret name %q", name)
	}
	if got, err := s.Load(ctx, key); err != nil || string(got) != "k" {
		t.Fatalf("Load = %q, %v", got, err)
	}
}

func TestListStatDelete(t *testing.T) {
	ctx := context.Background()
	s := newStorage(t)
	for _, k := range []string{
		"certificates/ca/a.example/a.example.crt",
		"certificates/ca/a.example/a.example.key",
		"certificates/ca/b.example/b.example.crt",
		"acme/ca/users/ops/ops.json",
	} {
		if err := s.Store(ctx, k, []byte(k)); err != nil {
			t.Fatal(err)
		}
	}
	got, err := s.List(ctx, "certificates", false)
	if err != nil || !reflect.DeepEqual(got, []string{"certificates/ca"}) {
		t.Fatalf("List non-recursive = %v, %v", got, err)
	}
	got, err = s.List(ctx, "certificates/ca", false)
	if err != nil || !reflect.DeepEqual(got, []string{"certificates/ca/a.example", "certificates/ca/b.example"}) {
		t.Fatalf("List children = %v, %v", got, err)
	}
	got, err = s.List(ctx, "certificates", true)
	if err != nil || len(got) != 3 {
		t.Fatalf("List recursive = %v, %v", got, err)
	}
	got, err = s.List(ctx, "", false)
	if err != nil || !reflect.DeepEqual(got, []string{"acme", "certificates"}) {
		t.Fatalf("List of the root = %v, %v", got, err)
	}
	if _, err := s.List(ctx, "nothing", true); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("List of nothing: %v", err)
	}
	// "certificates/c" is not a parent of "certificates/ca/...".
	if _, err := s.List(ctx, "certificates/c", true); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("List of a name prefix: %v", err)
	}

	info, err := s.Stat(ctx, "certificates/ca/a.example/a.example.crt")
	if err != nil || !info.IsTerminal || info.Size == 0 || info.Modified.IsZero() {
		t.Fatalf("Stat of a value = %+v, %v", info, err)
	}
	info, err = s.Stat(ctx, "certificates/ca")
	if err != nil || info.IsTerminal {
		t.Fatalf("Stat of a directory = %+v, %v", info, err)
	}
	if _, err := s.Stat(ctx, "nothing"); !errors.Is(err, fs.ErrNotExist) {
		t.Fatalf("Stat of nothing: %v", err)
	}

	if err := s.Delete(ctx, "certificates/ca/a.example"); err != nil {
		t.Fatal(err)
	}
	if s.Exists(ctx, "certificates/ca/a.example/a.example.key") {
		t.Fatal("Delete of a directory left a key below it")
	}
	if !s.Exists(ctx, "certificates/ca/b.example/b.example.crt") {
		t.Fatal("Delete removed a sibling")
	}
	if err := s.Delete(ctx, "certificates/ca/a.example"); err != nil {
		t.Fatal("Delete of a missing key:", err)
	}
}

func TestDeleteRefusesTheRoot(t *testing.T) {
	ctx := context.Background()
	s := newStorage(t)
	keys := []string{"certificates/ca/a.example/a.example.crt", "acme/ca/users/ops/ops.json"}
	for _, k := range keys {
		if err := s.Store(ctx, k, []byte(k)); err != nil {
			t.Fatal(err)
		}
	}
	for _, root := range []string{"", "/", "//"} {
		if err := s.Delete(ctx, root); !errors.Is(err, errDeleteRoot) {
			t.Fatalf("Delete(%q) = %v, want errDeleteRoot", root, err)
		}
	}
	for _, k := range keys {
		if !s.Exists(ctx, k) {
			t.Fatalf("Delete of the root removed %q", k)
		}
	}
}

func TestLockIsExclusive(t *testing.T) {
	ctx := context.Background()
	a := newStorage(t)
	b := &Storage{Namespace: "edge", client: a.client, holder: "edge-other"}
	if err := b.setup(); err != nil {
		t.Fatal(err)
	}
	if err := a.Lock(ctx, "issue_cert_staging.ruum.space"); err != nil {
		t.Fatal(err)
	}
	short, cancel := context.WithTimeout(ctx, 3*leaseDuration)
	defer cancel()
	// The renewal keeps a's lease fresh past its duration: b waits until its context ends.
	if err := b.Lock(short, "issue_cert_staging.ruum.space"); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("second Lock: %v, want DeadlineExceeded", err)
	}
	// Another name is free.
	if err := b.Lock(ctx, "issue_cert_dav.staging.ruum.space"); err != nil {
		t.Fatal(err)
	}
	if err := a.Unlock(ctx, "issue_cert_staging.ruum.space"); err != nil {
		t.Fatal(err)
	}
	done, cancel2 := context.WithTimeout(ctx, 2*time.Second)
	defer cancel2()
	if err := b.Lock(done, "issue_cert_staging.ruum.space"); err != nil {
		t.Fatal("Lock after Unlock:", err)
	}
	if err := b.Unlock(ctx, "issue_cert_staging.ruum.space"); err != nil {
		t.Fatal(err)
	}
	if err := b.Unlock(ctx, "issue_cert_staging.ruum.space"); err == nil {
		t.Fatal("a second Unlock succeeded")
	}
}

func TestExpiredLeaseIsTakenOver(t *testing.T) {
	ctx := context.Background()
	s := newStorage(t)
	old := metav1.NewMicroTime(time.Now().Add(-10 * time.Minute))
	holder := "a-dead-pod/0"
	seconds := int32(60)
	_, err := s.client.CoordinationV1().Leases("edge").Create(ctx, &coordinationv1.Lease{
		ObjectMeta: metav1.ObjectMeta{Name: leaseName("stale"), Namespace: "edge"},
		Spec: coordinationv1.LeaseSpec{
			HolderIdentity: &holder, LeaseDurationSeconds: &seconds, AcquireTime: &old, RenewTime: &old,
		},
	}, metav1.CreateOptions{})
	if err != nil {
		t.Fatal(err)
	}
	quick, cancel := context.WithTimeout(ctx, time.Second)
	defer cancel()
	if err := s.Lock(quick, "stale"); err != nil {
		t.Fatal("an expired lease was not taken over:", err)
	}
	if err := s.Unlock(ctx, "stale"); err != nil {
		t.Fatal(err)
	}
}

func TestUnlockLeavesAnotherHoldersLease(t *testing.T) {
	ctx := context.Background()
	s := newStorage(t)
	if err := s.Lock(ctx, "x"); err != nil {
		t.Fatal(err)
	}
	// Another pod took the lease over (as after an expiry).
	leases := s.client.CoordinationV1().Leases("edge")
	lease, err := leases.Get(ctx, leaseName("x"), metav1.GetOptions{})
	if err != nil {
		t.Fatal(err)
	}
	other := "edge-other/1"
	lease.Spec.HolderIdentity = &other
	if _, err := leases.Update(ctx, lease, metav1.UpdateOptions{}); err != nil {
		t.Fatal(err)
	}
	if err := s.Unlock(ctx, "x"); err != nil {
		t.Fatal(err)
	}
	if _, err := leases.Get(ctx, leaseName("x"), metav1.GetOptions{}); err != nil {
		t.Fatal("Unlock deleted another holder's lease:", err)
	}
}

func TestCaddyfile(t *testing.T) {
	var s Storage
	d := caddyfile.NewTestDispenser("kubernetes_secret {\n\tnamespace edge\n}")
	if err := s.UnmarshalCaddyfile(d); err != nil || s.Namespace != "edge" {
		t.Fatalf("namespace %q, %v", s.Namespace, err)
	}
	for _, bad := range []string{
		"kubernetes_secret extra",
		"kubernetes_secret {\n\tnamespace\n}",
		"kubernetes_secret {\n\tnamespace a b\n}",
		"kubernetes_secret {\n\tbucket x\n}",
	} {
		var s Storage
		if err := s.UnmarshalCaddyfile(caddyfile.NewTestDispenser(bad)); err == nil {
			t.Errorf("accepted %q", bad)
		}
	}
	bad := &Storage{Namespace: "Edge_1", client: fake.NewClientset(), holder: "x"}
	if err := bad.setup(); err == nil {
		t.Error("accepted a namespace that is not a DNS label")
	}
}

# ruum-egress: the egress proxy

The only way from an app or build sandbox to the internet (decision 23, decision 0006 answer 5
and section G, STEP3 "How an app runs" rule 2). A small Rust `CONNECT` proxy:

- It identifies the app **by the network the connection comes from** (the source address inside
  a registered subnet), never by a header or a credential that the app sends.
- It accepts `CONNECT host:port` only. Every other method gets `405`, so the proxy never sees a
  path, a query string, a header of the app's request or a body.
- It checks the host against the network's `egress.allow`, resolves the name **once**, refuses
  the request when **any** answer is not a public address, and connects to the checked address.
  It never resolves again for that tunnel, so a DNS answer that changes cannot redirect it.
  (When the first checked address does not answer, it tries the other answers of the same
  resolution, which all passed the rules.)
- It limits tunnels per identity (at once, and a rate), closes idle and old tunnels, counts bytes
  per identity and hour, and logs one line per tunnel (no path, no query string, no header).

Plain `http://` is not proxied: an app that calls a plain HTTP API through the proxy gets `405`
(absolute-form requests carry the path). UDP is not proxied at all.

## Proxy listener

`RUUM_EGRESS_LISTEN` (default `0.0.0.0:3128`). The app sets `HTTPS_PROXY=http://<proxy>:3128`.

| Answer | When |
|---|---|
| `200 Connection Established` | The tunnel is open. |
| `403` with `X-Ruum-Egress-Deny: <reason>` | The proxy refused. Reasons: `address`, `allow`, `port`, `unknown_source`, `limit`. |
| `405` with `X-Ruum-Egress-Deny: method` | Not a `CONNECT` request. |
| `400` | The request head is not a valid `CONNECT host:port` request (no deny header). |
| `502` (no deny header) | The name did not resolve, or the checked address did not answer. |

A refusal counts only with the deny header (decision 0006 G); a test that sees a `502` or a
timeout has not seen a refusal.

### Address rules (decision 0006 G point 1)

Checked after resolution on every answer, and on IP literals the same way. Host names given as
numbers are parsed the way a resolver does (`inet_aton`: decimal `2852039166`, octal `0251.0376.0251.0376`,
hex `0xa9fea9fe`, short forms `127.1`) and then checked like any answer. The proxy refuses:

- IPv4: `0.0.0.0/8`, `10.0.0.0/8`, `100.64.0.0/10` (CGNAT), `127.0.0.0/8`, `169.254.0.0/16`
  (link-local, the metadata address `169.254.169.254`), `172.16.0.0/12`, `192.0.0.0/24`,
  `192.0.2.0/24`, `192.88.99.0/24`, `192.168.0.0/16`, `198.18.0.0/15` (benchmarking),
  `198.51.100.0/24`, `203.0.113.0/24`, `224.0.0.0/4` (multicast), `240.0.0.0/4` and
  `255.255.255.255` (broadcast).
- IPv6: everything outside global unicast `2000::/3` (so loopback `::1`, `::`, IPv4-compatible
  `::a.b.c.d`, NAT64 `64:ff9b::/96`, unique local `fc00::/7` with the metadata address
  `fd00:ec2::254`, link-local `fe80::/10` and multicast `ff00::/8`), and inside it
  `2001::/23` (IETF protocol assignments: Teredo `2001::/32`, benchmarking `2001:2::/48`,
  ORCHID), documentation `2001:db8::/32` and `3fff::/20` (exactly `3fff:0000::` to
  `3fff:0fff:...`), and 6to4 `2002::/16`.
- An IPv4-mapped answer (`::ffff:a.b.c.d`) is checked, denied and connected to as its IPv4
  form, so `::ffff:<host IP>` meets the deny entry `<host IP>`.
- Every entry of `EGRESS_DENY`: the platform ranges and the host's public addresses (addresses
  or CIDR prefixes; the host's IPv6 is a `/64`).

## Control listener

A separate listener that app networks can never reach. `RUUM_EGRESS_CONTROL` is either
`unix:/path/to/socket` (the normal case: a socket on a volume that only the server mounts) or
`<loopback IP>:<port>`. The proxy refuses to start when the control address is on a network it
also serves: a TCP control address must be loopback, and then the proxy listener must be on a
specific address that is neither unspecified nor loopback.

Every request carries `Authorization: Bearer <token>`. The token is read from the file
`RUUM_EGRESS_CONTROL_TOKEN_FILE` (at least 32 bytes; a trailing newline is removed), never from an
environment value, and is never logged. The comparison takes constant time.

The types are in `ruum_egress::api` (the runtime driver's `EgressRegistry` client uses them).

### `PUT /v1/networks/{id}`

Registers or replaces one app network. `id`: 1 to 128 characters of `A-Z a-z 0-9 _ . -` (the
driver uses the Docker network name or id).

```json
{
  "subnet": "172.30.5.0/24",
  "identity": {
    "tenant": "0190f0e2-...",
    "app": "0190f0e3-...",
    "deployment": "0190f0e4-...",
    "environment": "live"
  },
  "allow": ["registry.npmjs.org", "*.example.com"],
  "limits": { "max_tunnels": 16, "per_minute": 60, "burst": 10, "idle_timeout_secs": 120, "max_lifetime_secs": 900 }
}
```

- `allow` is `egress.allow` of the manifest: at most 50 entries matching
  `^(\*\.)?([a-z0-9-]+\.)+[a-z]{2,63}$` (entries are lowercased first). Absent or `null`: every
  host passes the allow step. `[]`: no host passes. `*.example.com` matches one or more labels
  below `example.com`, never `example.com` itself and never `evilexample.com`. Matching ignores
  case and one trailing dot. The address rules still apply after the allow step.
- `limits` is optional; each field may only lower the proxy's configured limit.
- Answers: `204` (registered), `400 invalid_request`, `401 unauthorized`, `409 subnet_overlap`
  (another id holds an overlapping subnet), `409 too_many_networks` (`RUUM_EGRESS_MAX_NETWORKS`).

### `DELETE /v1/networks/{id}`

Removes the registration. `204` (also when it did not exist). Open tunnels of the network keep
running until they end or hit a limit; new connections from the subnet get `unknown_source`.

### `GET /v1/usage`

Drains the byte counters (the core's metering, card I-8, calls it):

```json
{
  "entries": [
    { "identity": { ... }, "hour": "2026-11-15T10:00:00Z", "bytes_up": 1200, "bytes_down": 53000, "tunnels": 3, "denied": 1 }
  ],
  "overflow": { "bytes_up": 0, "bytes_down": 0 }
}
```

Counters are per identity and hour (UTC), never per visitor. A second call returns only what was
counted after the first. `overflow` holds bytes that found no free counter (the proxy bounds its
memory at 65,536 entries between two drains).

Errors use the API's form: `{"error": {"code": "...", "message": "...", "hint": "..."}}`.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `RUUM_EGRESS_LISTEN` | `0.0.0.0:3128` | The proxy listener (the service port on app networks). |
| `RUUM_EGRESS_CONTROL` | required | `unix:/path` or `<loopback IP>:<port>`. |
| `RUUM_EGRESS_CONTROL_TOKEN_FILE` | required | The file with the control token (32 bytes or more). |
| `RUUM_EGRESS_PORTS` | `443` | The ports a `CONNECT` may name, separated by commas. |
| `EGRESS_DENY` | required, unless dev mode | The platform ranges and the host's public addresses: addresses or CIDR prefixes, separated by commas or spaces. An entry that does not parse stops the start. |
| `RUUM_EGRESS_DEV` | unset | `1` allows an empty `EGRESS_DENY` (the start log says dev mode). |
| `RUUM_EGRESS_MAX_TUNNELS` | `64` | Open tunnels per identity at once. |
| `RUUM_EGRESS_PER_MINUTE` | `120` | New tunnels per identity per minute (token bucket refill). |
| `RUUM_EGRESS_BURST` | `30` | New tunnels per identity at once (token bucket size). |
| `RUUM_EGRESS_IDLE_TIMEOUT_SECS` | `300` | A tunnel with no bytes for this long closes. |
| `RUUM_EGRESS_MAX_LIFETIME_SECS` | `3600` | A tunnel closes after this long. |
| `RUUM_EGRESS_CONNECT_TIMEOUT_SECS` | `10` | Resolving plus connecting to the upstream. |
| `RUUM_EGRESS_MAX_NETWORKS` | `4096` | Registered networks at most. |
| `RUUM_EGRESS_MAX_CONNECTIONS` | `4096` | Client connections at once, all sources together (the proxy's memory bound). |
| `RUUM_EGRESS_LOG` | `info` | The log filter (`tracing` syntax). |

The request head must arrive within 10 seconds and be at most 8 KiB.

## Logs

One line per request at `info`, target `ruum_egress::tunnel`: the source network, the identity
(tenant, app, deployment, environment), host, port, resolved address, bytes up and down, duration,
status and deny reason. Never a path, query string, header or body: the proxy reads only the
first line of the request head, and an absolute-form request is logged with host `-`.

## Tests

- `cargo test -p ruum-egress`: the conformance suite (`src/conformance.rs`) against the
  in-memory policy (`tests/memory.rs`) and against the running proxy on `127.0.0.1` with a fake
  resolver (`tests/binary.rs`, which also starts the built binary for the start-up checks, the
  control listener and the literal-address cases). Linux only for the source-address cases (the
  whole `127.0.0.0/8` is loopback there).
- The container check on the build server (acceptance test 2):
  `crates/ruum-egress/tests/container_check.sh` builds the image, starts it on an
  `internal: true`, `inhibit_ipv4` network and probes from a container on that network
  (npm reached; the metadata addresses, the host's public IPv4, its IPv6 and /64 neighbour, and
  encoded forms refused with `deny=address`; the allow-list; an unregistered network; no direct
  way out; the token and paths not in the log; no test hook in the release binary). It needs
  Docker and the internet; it is not part of `cargo test`. Its containers, networks and image
  carry the label `ruum.test=s3w3r2e` and are removed at the end.
- The test-only switch that treats one loopback socket address as public
  (`MemoryPolicy::set_test_public`) exists only with the `test-hooks` feature, which only the
  crate's dev-dependency on itself turns on. No environment variable reaches it, and the release
  build of the image does not contain it (the container check looks).
- Mutations: `crates/ruum-testkit/mutations/egress-*.sh`
  (`crates/ruum-testkit/scripts/mutation_check.sh egress-...`).

## Image

`deploy/images/ruum-egress/Dockerfile`: a multi-stage build on pinned bases, a
distroless non-root runtime, no shell, read-only root friendly.

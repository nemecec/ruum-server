//! The control interface of the egress proxy: the request and response types of the control
//! listener (`PUT /v1/networks/{id}`, `DELETE /v1/networks/{id}`, `GET /v1/usage`), and the deny
//! answer of the proxy listener. The runtime driver's `EgressRegistry` client (card R-2a) uses
//! these types. The crate README describes the interface.
//!
//! Changes are additive only: a new optional field, never a renamed or removed one.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// The header of the proxy's own refusal. Its value is a [`DenyReason`]. A refusal counts only
/// with this header (decision 0006 G); a failed upstream is `502` without it.
pub const DENY_HEADER: &str = "X-Ruum-Egress-Deny";

/// The path prefix of the network registrations: `PUT` and `DELETE` on `{NETWORKS_PATH}/{id}`.
pub const NETWORKS_PATH: &str = "/v1/networks";

/// The path of the byte counters: `GET` drains them.
pub const USAGE_PATH: &str = "/v1/usage";

/// The most entries `allow` may have (`contracts/manifests/app.schema.json`, `egress.allow`).
pub const MAX_ALLOW_ENTRIES: usize = 50;

/// Who the traffic of a network belongs to. The same fields as
/// `ruum_core::runtime::WorkloadIdentity` (the proxy does not depend on `ruum-core`, which is too
/// heavy for a small proxy image), with the same JSON form: the ids are plain UUIDs and the
/// environment is `live` or `preview`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadIdentity {
    pub tenant: Uuid,
    /// The app node.
    pub app: Uuid,
    /// The deployment (`app_deployments.id`).
    pub deployment: Uuid,
    pub environment: Environment,
}

/// The environment of a deployment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    Live,
    Preview,
}

impl Environment {
    pub fn as_str(self) -> &'static str {
        match self {
            Environment::Live => "live",
            Environment::Preview => "preview",
        }
    }
}

/// The body of `PUT /v1/networks/{id}`: one app network. A `PUT` of an existing id replaces it.
///
/// The proxy identifies a connection only by its source address: the registration whose
/// `subnet` holds it. Subnets of two registrations never overlap (`409 subnet_overlap`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkRegistration {
    /// The network's subnet as a CIDR prefix (`172.30.5.0/24`, `fd12:3456::/64`). An IPv4
    /// prefix is at least `/8`, an IPv6 prefix at least `/32`.
    pub subnet: String,
    pub identity: WorkloadIdentity,
    /// `egress.allow` of the manifest: host names, exact or `*.` wildcards, at most 50. Absent
    /// (or `null`): every host passes the allow step. An empty list: no host passes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    /// Lower limits for this network. A value may only lower the proxy's configured limit; a
    /// higher value is lowered to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<Limits>,
}

/// Per-identity limits. Every field is optional; an absent field keeps the proxy's default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Open tunnels at the same time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tunnels: Option<u32>,
    /// New client connections per minute, refused requests included (the refill of a token
    /// bucket).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_minute: Option<u32>,
    /// The size of the token bucket: new client connections at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst: Option<u32>,
    /// A tunnel with no byte in either direction for this long is closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout_secs: Option<u64>,
    /// A tunnel is closed after this long, busy or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lifetime_secs: Option<u64>,
}

/// The answer of `GET /v1/usage`. The call drains the counters: a second call returns only what
/// was counted after the first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReport {
    pub entries: Vec<UsageEntry>,
    /// Bytes that found no free counter (the proxy bounds its memory). Zero in normal operation;
    /// the metering adds them to the platform, never to a tenant.
    pub overflow: ByteCounts,
}

/// The bytes of one identity in one hour (UTC). Counters per identity, never per visitor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageEntry {
    pub identity: WorkloadIdentity,
    /// The start of the hour.
    #[serde(with = "time::serde::rfc3339")]
    pub hour: OffsetDateTime,
    /// From the app to the internet.
    pub bytes_up: u64,
    /// From the internet to the app.
    pub bytes_down: u64,
    /// Tunnels opened in this hour.
    pub tunnels: u64,
    /// Requests refused in this hour (any deny reason but `unknown_source`).
    pub denied: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteCounts {
    pub bytes_up: u64,
    pub bytes_down: u64,
}

/// The error body of the control listener (the API's error form).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub error: ApiErrorBody,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiErrorBody {
    /// `unauthorized`, `invalid_request`, `subnet_overlap`, `too_many_networks`, `not_found`.
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Why the proxy refused a request: the value of [`DENY_HEADER`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// A resolved address (or an IP literal) is loopback, private, link-local, metadata,
    /// platform, the host's own, or otherwise not public.
    Address,
    /// The host is not on the network's `egress.allow` list.
    Allow,
    /// The port is not in `RUUM_EGRESS_PORTS`.
    Port,
    /// Not a `CONNECT` request (`405`).
    Method,
    /// The source address is in no registered network.
    UnknownSource,
    /// A limit of the identity (tunnels at once or the connection rate).
    Limit,
}

impl DenyReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DenyReason::Address => "address",
            DenyReason::Allow => "allow",
            DenyReason::Port => "port",
            DenyReason::Method => "method",
            DenyReason::UnknownSource => "unknown_source",
            DenyReason::Limit => "limit",
        }
    }

    pub fn parse(s: &str) -> Option<DenyReason> {
        Some(match s {
            "address" => DenyReason::Address,
            "allow" => DenyReason::Allow,
            "port" => DenyReason::Port,
            "method" => DenyReason::Method,
            "unknown_source" => DenyReason::UnknownSource,
            "limit" => DenyReason::Limit,
            _ => return None,
        })
    }

    /// The status line of the refusal: `405` for a method, `403` for every other reason.
    pub fn status(self) -> u16 {
        match self {
            DenyReason::Method => 405,
            _ => 403,
        }
    }
}

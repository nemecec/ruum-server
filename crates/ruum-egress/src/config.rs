//! The configuration from the environment, and the start-up checks.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use crate::addr::DenyList;
use crate::control::ControlToken;
use crate::limits::LimitConfig;

/// Where the control listener is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ControlAddr {
    Unix(PathBuf),
    Tcp(SocketAddr),
}

impl std::fmt::Display for ControlAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlAddr::Unix(p) => write!(f, "unix:{}", p.display()),
            ControlAddr::Tcp(a) => write!(f, "{a}"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub control: ControlAddr,
    pub token: ControlToken,
    pub ports: Vec<u16>,
    pub deny: DenyList,
    pub dev: bool,
    pub limits: LimitConfig,
    pub connect_timeout: Duration,
    pub head_timeout: Duration,
    pub max_networks: usize,
    pub max_connections: usize,
}

/// Why the proxy does not start. No variant holds the control token.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0} is not set")]
    Missing(&'static str),
    #[error(
        "EGRESS_DENY is empty: set the platform ranges and the host's public addresses (RUUM_EGRESS_DEV=1 allows an empty list on a laptop)"
    )]
    EmptyDeny,
    #[error("{0}")]
    BadDeny(String),
    #[error(
        "the control address {control} is on a network the proxy also serves ({listen}): use unix:/path, or a loopback control address with a proxy listener on a specific non-loopback address"
    )]
    ControlOnServedNetwork { control: String, listen: SocketAddr },
}

fn num<T>(
    get: &impl Fn(&str) -> Option<String>,
    name: &'static str,
    default: T,
    min: T,
) -> Result<T, ConfigError>
where
    T: std::str::FromStr + PartialOrd + std::fmt::Display,
{
    match get(name) {
        None => Ok(default),
        Some(v) => match v.trim().parse::<T>() {
            Ok(n) if n >= min => Ok(n),
            _ => Err(ConfigError::Invalid(format!(
                "{name} must be a number of at least {min}"
            ))),
        },
    }
}

/// The control address must be on no network the proxy serves. A Unix socket is never on one;
/// a TCP control address must be loopback, and the proxy must then listen on a specific address
/// that is neither unspecified (every network, loopback too) nor loopback.
pub fn validate_control(listen: SocketAddr, control: &ControlAddr) -> Result<(), ConfigError> {
    let ControlAddr::Tcp(c) = control else {
        return Ok(());
    };
    let l = listen.ip();
    if !c.ip().is_loopback() || l.is_unspecified() || l.is_loopback() || *c == listen {
        return Err(ConfigError::ControlOnServedNetwork {
            control: control.to_string(),
            listen,
        });
    }
    Ok(())
}

impl Config {
    /// Reads the configuration through `get` (the environment in the binary). Reads the token
    /// file. Every error stops the start.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
        let get = |name: &str| get(name).filter(|v| !v.is_empty());
        let listen: SocketAddr = match get("RUUM_EGRESS_LISTEN") {
            None => SocketAddr::from(([0, 0, 0, 0], 3128)),
            Some(v) => v.trim().parse().map_err(|_| {
                ConfigError::Invalid(format!("RUUM_EGRESS_LISTEN `{v}` is not an address:port"))
            })?,
        };
        let control = match get("RUUM_EGRESS_CONTROL") {
            None => return Err(ConfigError::Missing("RUUM_EGRESS_CONTROL")),
            Some(v) => match v.trim().strip_prefix("unix:") {
                Some(p) if !p.is_empty() => ControlAddr::Unix(PathBuf::from(p)),
                Some(_) => {
                    return Err(ConfigError::Invalid(
                        "RUUM_EGRESS_CONTROL unix: needs a path".into(),
                    ));
                }
                None => ControlAddr::Tcp(v.trim().parse().map_err(|_| {
                    ConfigError::Invalid(format!(
                        "RUUM_EGRESS_CONTROL `{v}` is neither unix:/path nor address:port"
                    ))
                })?),
            },
        };
        validate_control(listen, &control)?;

        let token_file = get("RUUM_EGRESS_CONTROL_TOKEN_FILE")
            .ok_or(ConfigError::Missing("RUUM_EGRESS_CONTROL_TOKEN_FILE"))?;
        let raw = std::fs::read_to_string(&token_file).map_err(|e| {
            ConfigError::Invalid(format!(
                "RUUM_EGRESS_CONTROL_TOKEN_FILE {token_file}: {}",
                e.kind()
            ))
        })?;
        let value = raw.strip_suffix('\n').unwrap_or(&raw);
        let value = value.strip_suffix('\r').unwrap_or(value);
        let token = ControlToken::new(value).map_err(ConfigError::Invalid)?;

        let ports = match get("RUUM_EGRESS_PORTS") {
            None => vec![443],
            Some(v) => {
                let mut out = Vec::new();
                for p in v.split(',') {
                    match p.trim().parse::<u16>() {
                        Ok(n) if n != 0 => out.push(n),
                        _ => {
                            return Err(ConfigError::Invalid(format!(
                                "RUUM_EGRESS_PORTS entry `{}` is not a port",
                                p.trim()
                            )));
                        }
                    }
                }
                out
            }
        };

        let dev = get("RUUM_EGRESS_DEV").as_deref() == Some("1");
        let deny = DenyList::parse(&get("EGRESS_DENY").unwrap_or_default())
            .map_err(ConfigError::BadDeny)?;
        if deny.is_empty() && !dev {
            return Err(ConfigError::EmptyDeny);
        }

        let d = LimitConfig::default();
        let limits = LimitConfig {
            max_tunnels: num(&get, "RUUM_EGRESS_MAX_TUNNELS", d.max_tunnels, 1)?,
            per_minute: num(&get, "RUUM_EGRESS_PER_MINUTE", d.per_minute, 1)?,
            burst: num(&get, "RUUM_EGRESS_BURST", d.burst, 1)?,
            idle_timeout: Duration::from_secs(num(
                &get,
                "RUUM_EGRESS_IDLE_TIMEOUT_SECS",
                d.idle_timeout.as_secs(),
                1,
            )?),
            max_lifetime: Duration::from_secs(num(
                &get,
                "RUUM_EGRESS_MAX_LIFETIME_SECS",
                d.max_lifetime.as_secs(),
                1,
            )?),
        };
        Ok(Config {
            listen,
            control,
            token,
            ports,
            deny,
            dev,
            limits,
            connect_timeout: Duration::from_secs(num(
                &get,
                "RUUM_EGRESS_CONNECT_TIMEOUT_SECS",
                10,
                1,
            )?),
            head_timeout: Duration::from_secs(10),
            max_networks: num(&get, "RUUM_EGRESS_MAX_NETWORKS", 4096, 1)?,
            max_connections: num(&get, "RUUM_EGRESS_MAX_CONNECTIONS", 4096, 1)?,
        })
    }
}

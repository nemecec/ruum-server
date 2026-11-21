//! The tokens of the runtime API (decision 0006-10): the checks of workload tokens, of the
//! gateway's user tokens (`Ruum-User`) and of delegated tokens.
//!
//! The checks live in `ruum_core::apps::runtime_tokens`, because the core's internal routes for
//! the runtime API check every token again (review of R-3, required change 1); this module
//! re-exports them. The runtime API signs nothing: the core signs delegated tokens with the
//! workload key, so the runtime API checks workload and delegated tokens with the one public key
//! set of workload tokens (`RUUM_RUNTIME_WORKLOAD_JWKS_FILE`), each kind by its own `typ`.

pub use ruum_core::apps::runtime_tokens::*;

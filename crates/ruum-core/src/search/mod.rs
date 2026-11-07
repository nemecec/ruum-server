//! Search by name (`docs/DESIGN.md`, section 12.3). Step 1 searches node names in Postgres
//! (trigram index); results are filtered by permission with bulk checks. Full-text search of file
//! content comes in a later phase.
//!
//! Results are the live nodes whose name contains `q` (case-insensitive) that the caller can see,
//! best match first: an exact name, then names that start with `q`, then the rest (Postgres
//! orders the rest by trigram similarity, memory by length). `within` limits the search to a
//! folder that the caller can see; a folder the caller cannot see, or of another tenant, gives
//! no results. `type` matches `Node.type` (`nodes.type`), ignoring case: an exact type, or a
//! prefix that ends in `/` (decision 0003-7). `q` may be left out when `type`, `tag` or
//! `within` is given; the results are then ordered by name. The cursor is an offset into the
//! candidates (before the permission filter).

pub mod memory;
pub mod postgres;

use crate::actor::Context;
use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::ids::{NodeId, TagId};
use crate::nodes::{Node, NodeKind};
use crate::paging::{Page, PageRequest};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The longest query, in characters.
pub const MAX_QUERY_CHARS: usize = 255;

/// At most this many candidate batches are read for one page (so a page of a search where the
/// caller sees little stays cheap; the page may then be short, with a cursor).
pub const MAX_BATCHES: usize = 10;

/// A search by name.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Matches names that contain the text, case-insensitive. Optional when `node_type`, `tag`
    /// or `within` is given.
    pub q: Option<String>,
    pub kind: Option<NodeKind>,
    /// Matches `Node.type`, ignoring case: an exact type (`application/pdf`), or a prefix that
    /// ends in `/` (`image/`).
    #[serde(rename = "type")]
    pub node_type: Option<String>,
    /// Only nodes with this tag.
    pub tag: Option<TagId>,
    /// Only nodes inside this folder (at any depth).
    pub within: Option<NodeId>,
    #[serde(flatten)]
    pub page: PageRequest,
}

/// One search result.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchHit {
    pub node: Node,
    /// The display path, for example `/Projects/Garden/plan.txt`.
    pub path: String,
}

#[async_trait]
pub trait Search: Send + Sync {
    async fn by_name(&self, ctx: &Context, query: SearchQuery) -> Result<Page<SearchHit>>;
}

/// The longest `type` filter, in characters.
pub const MAX_TYPE_CHARS: usize = 255;

/// The checked filters of a query: the name text (`None`: no name filter) and the type filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filters {
    pub q: Option<String>,
    pub node_type: Option<TypeFilter>,
}

/// The `type` filter, lower case.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeFilter {
    /// The whole type, for example `application/pdf`.
    Exact(String),
    /// A prefix that ends in `/`, for example `image/`.
    Prefix(String),
}

impl TypeFilter {
    /// Whether a node type matches.
    pub fn matches(&self, node_type: Option<&str>) -> bool {
        let Some(t) = node_type.map(str::to_lowercase) else {
            return false;
        };
        match self {
            TypeFilter::Exact(e) => t == *e,
            TypeFilter::Prefix(p) => t.starts_with(p.as_str()),
        }
    }
}

/// Checks the filters of a query: `q` is checked by [`query_text`] when given; without `q`,
/// one of `type`, `tag` or `within` is needed (`validation_failed`).
pub fn filters(query: &SearchQuery) -> Result<Filters> {
    let q = query.q.as_deref().map(query_text).transpose()?;
    let node_type = query
        .node_type
        .as_deref()
        .map(|t| {
            let t = t.trim().to_lowercase();
            if t.is_empty()
                || t.chars().count() > MAX_TYPE_CHARS
                || t.chars().any(|c| c.is_control() || c.is_whitespace())
            {
                return Err(Error::Api(
                    ApiError::new(
                        ErrorCode::ValidationFailed,
                        format!("type is 1 to {MAX_TYPE_CHARS} characters without spaces"),
                    )
                    .with_hint(
                        "send a MIME type such as application/pdf, or a prefix such as image/",
                    ),
                ));
            }
            Ok(if t.ends_with('/') {
                TypeFilter::Prefix(t)
            } else {
                TypeFilter::Exact(t)
            })
        })
        .transpose()?;
    if q.is_none() && node_type.is_none() && query.tag.is_none() && query.within.is_none() {
        return Err(Error::Api(
            ApiError::new(
                ErrorCode::ValidationFailed,
                "send q, or at least one of type, tag and within",
            )
            .with_hint("q matches names; type matches the MIME type, for example application/pdf"),
        ));
    }
    Ok(Filters { q, node_type })
}

/// The normalized query text (NFC, lower case) after the checks of the contract.
pub fn query_text(q: &str) -> Result<String> {
    use unicode_normalization::UnicodeNormalization;
    let n: String = q.nfc().collect();
    let len = n.chars().count();
    if n.trim().is_empty() || len > MAX_QUERY_CHARS {
        return Err(Error::Api(
            ApiError::new(
                ErrorCode::ValidationFailed,
                format!("q is 1 to {MAX_QUERY_CHARS} characters"),
            )
            .with_hint("send part of a node name in q"),
        ));
    }
    Ok(n.to_lowercase())
}

/// The rank of a name for `q` (lower is better): exact, prefix, other; then shorter names.
pub fn rank(name: &str, q: &str) -> (u8, usize) {
    let n = name.to_lowercase();
    let class = if n == q {
        0
    } else if n.starts_with(q) {
        1
    } else {
        2
    };
    (class, n.chars().count())
}

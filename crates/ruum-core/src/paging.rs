//! Cursor paging. Lists return `{ items, next_cursor }`; the cursor is opaque to the client.

use serde::{Deserialize, Serialize};

/// The largest page size that a list returns.
pub const MAX_PAGE_SIZE: u32 = 1000;
/// The page size when the client does not ask for one.
pub const DEFAULT_PAGE_SIZE: u32 = 100;

/// An opaque cursor (base64url of an internal position).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cursor(pub String);

/// A request for one page.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PageRequest {
    pub cursor: Option<Cursor>,
    pub limit: Option<u32>,
}

impl PageRequest {
    /// The page size, clamped to `1..=MAX_PAGE_SIZE`.
    pub fn limit(&self) -> u32 {
        self.limit
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE)
    }
}

/// One page of results.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// The cursor for the next page, or `None` on the last page.
    pub next_cursor: Option<Cursor>,
}

impl<T> Page<T> {
    pub fn empty() -> Self {
        Self {
            items: Vec::new(),
            next_cursor: None,
        }
    }
}

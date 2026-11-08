//! Access grants and approvals (cards A-1, A-2): the `access-grants` and `approvals`
//! operations. Declared by card D-1. A-1 builds [`grants`]; [`approvals`] stays empty until A-2,
//! so A-2 never edits this file (the pattern of decision 0004-33).

pub mod approvals;
pub mod grants;

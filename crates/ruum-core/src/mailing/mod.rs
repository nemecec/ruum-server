//! Share and invitation emails (decision 0003-3, contract need C-13) and the rate counters of
//! decision 0003-2 and 0003-3.
//!
//! - [`outbox`]: the Postgres mail outbox. A change (a grant with `notify`, an invitation)
//!   writes a row in its own transaction; the worker sends it after the commit, retries, gives
//!   up after 24 hours, and cleans up old rows. The in-memory stores send at once instead (as
//!   they write relationships straight to the engine).
//! - [`rate`]: fixed-window counters, in Postgres (`rate_counters`, shared by every server
//!   process) and in memory.
//!
//! The mail templates and the mailer are in [`crate::tenants::mail`].

pub mod outbox;
pub mod rate;

use crate::authz::Role;
use crate::tenants::MemberRole;
use crate::tenants::mail::{self, InvitationMail, Mail, ShareMail};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

/// The fields of a share mail, without the link (the worker adds it from its public URL).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareFields {
    pub sharer: String,
    pub item: String,
    pub folder: bool,
    pub role: Role,
    #[serde(default)]
    pub message: Option<String>,
    pub email_grant: bool,
    #[serde(default)]
    pub reply_to: Option<String>,
}

impl ShareFields {
    /// The mail to `to` for the item `node` of `tenant`.
    pub fn render(&self, to: &str, public_url: &str, tenant: uuid::Uuid, node: uuid::Uuid) -> Mail {
        let link = mail::item_link(public_url, tenant, node, self.folder);
        mail::share_mail(&ShareMail {
            to,
            sharer: &self.sharer,
            item: &self.item,
            folder: self.folder,
            role: self.role,
            message: self.message.as_deref(),
            link: &link,
            email_grant: self.email_grant,
            reply_to: self.reply_to.as_deref(),
        })
    }
}

/// The fields of an invitation mail, without the link (the worker mints the token).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitationFields {
    pub space: String,
    pub inviter: String,
    pub role: MemberRole,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(default)]
    pub message: Option<String>,
}

impl InvitationFields {
    pub fn render(&self, to: &str, accept_url: &str) -> Mail {
        mail::invitation_mail(&InvitationMail {
            to,
            space: &self.space,
            inviter: &self.inviter,
            role: self.role,
            expires_at: self.expires_at,
            accept_url,
            message: self.message.as_deref(),
        })
    }
}

/// `addGrant` calls per caller user (agents count against their user) and tenant, per window
/// (decision 0003-2).
pub const ADD_GRANT_PER_USER: i64 = 60;
/// `addGrant` calls per tenant, per window.
pub const ADD_GRANT_PER_TENANT: i64 = 600;
/// The window of the `addGrant` limits: 10 minutes.
pub const ADD_GRANT_WINDOW_SECONDS: i64 = 600;
/// Share mails per sharing user and tenant, per window (decision 0003-3).
pub const SHARE_MAIL_PER_USER: i64 = 50;
/// Share mails per tenant, per window.
pub const SHARE_MAIL_PER_TENANT: i64 = 500;
/// The window of the share mail limits: one hour.
pub const SHARE_MAIL_WINDOW_SECONDS: i64 = 3600;
/// At most one share mail per recipient and node in this time.
pub const SHARE_MAIL_REPEAT_SECONDS: i64 = 600;

/// The counter keys.
pub fn add_grant_user_key(user: uuid::Uuid) -> String {
    format!("add_grant:user:{user}")
}
pub const ADD_GRANT_TENANT_KEY: &str = "add_grant:tenant";
pub fn share_mail_user_key(user: uuid::Uuid) -> String {
    format!("share_mail:user:{user}")
}
pub const SHARE_MAIL_TENANT_KEY: &str = "share_mail:tenant";

/// The answer of a refused `addGrant` over the limit.
pub fn add_grant_rate_limited() -> crate::error::Error {
    crate::error::Error::Api(
        crate::error::ApiError::new(
            crate::error::ErrorCode::RateLimited,
            "too many grants in a short time",
        )
        .with_hint(format!(
            "at most {ADD_GRANT_PER_USER} addGrant calls per person and {ADD_GRANT_PER_TENANT} per \
             space in 10 minutes; wait and try again"
        )),
    )
}

//! Typed IDs. Every ID is a UUIDv7 (time-ordered). An ID never changes.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, sqlx::Type)]
        #[serde(transparent)]
        #[sqlx(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Makes a new UUIDv7 ID.
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }

            /// The raw UUID.
            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }

        impl From<Uuid> for $name {
            fn from(u: Uuid) -> Self {
                Self(u)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Uuid {
                id.0
            }
        }
    };
}

id_type!(
    /// A tenant: an organisation that pays, or a private person.
    TenantId
);
id_type!(
    /// A node in the tree: folder, file, link or typed node.
    NodeId
);
id_type!(
    /// Any principal: a user, group, app or agent. The kind is in [`crate::PrincipalRef`].
    PrincipalId
);
id_type!(
    /// A user (a person). A user ID is also a principal ID.
    UserId
);
id_type!(
    /// An agent principal. An agent ID is also a principal ID.
    AgentId
);
id_type!(
    /// An API key of an agent.
    AgentKeyId
);
id_type!(
    /// A version of a node.
    VersionId
);
id_type!(
    /// A tenant tag.
    TagId
);
id_type!(
    /// A grant of a role on a node to a principal.
    GrantId
);
id_type!(
    /// A secret share link.
    ShareLinkId
);
id_type!(
    /// An invitation to join a tenant.
    InvitationId
);
id_type!(
    /// A resumable upload session.
    UploadId
);

impl From<UserId> for PrincipalId {
    fn from(id: UserId) -> Self {
        Self(id.0)
    }
}

impl From<AgentId> for PrincipalId {
    fn from(id: AgentId) -> Self {
        Self(id.0)
    }
}

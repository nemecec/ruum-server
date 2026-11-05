-- Ruum step 1 (Core): tenants, identity, agents, nodes, versions, blobs, tags, sharing,
-- journal, audit and idempotency keys. See docs/DESIGN.md, sections 2-5.
--
-- Rules for every table:
-- - `tenant_id` on every row; every query filters by it. Postgres row-level security on
--   `tenant_id` is a second guard (section 2.6).
-- - IDs are UUIDv7, made by the application (Postgres 17 has no uuidv7()).
-- - Times are timestamptz in UTC.
-- - Every write records the actor and the user it acted for (section 3.4):
--   `*_kind` (principal_kind), `*_by` / `actor` (uuid) and `*_obo` / `on_behalf_of` (uuid, null when
--   the actor is the user).
--
-- Row-level security: the application sets, per transaction,
--   set local ruum.tenant_id = '<uuid>';   -- the tenant of the request
--   set local ruum.user_id   = '<uuid>';   -- the effective user (for "shared with me")
--   set local ruum.system    = 'on';       -- only for the few cross-tenant lookups: sign-in,
--                                          -- key exchange, share link and invitation tokens
-- The policies are FORCEd, so they also apply to the table owner. Superusers (tests with
-- #[sqlx::test]) bypass them.

create extension if not exists pg_trgm;

-- ---------------------------------------------------------------------------- types

create type principal_kind as enum ('user', 'group', 'app', 'agent', 'share_link');
create type tenant_kind as enum ('personal', 'team');
create type plan as enum ('free', 'personal', 'team');
create type member_role as enum ('guest', 'member', 'admin', 'owner');
create type node_kind as enum ('folder', 'file', 'link', 'typed');
create type link_kind as enum ('shortcut', 'member');
create type grant_role as enum ('view', 'comment', 'edit', 'manage', 'share');
create type grantee_kind as enum ('user', 'email', 'tenant_members');
create type upload_status as enum ('open', 'completed', 'aborted', 'expired');
create type agent_disabled_reason as enum ('by_owner', 'by_admin', 'owner_left');

-- ---------------------------------------------------------------------------- RLS helpers

create function ruum_current_tenant() returns uuid
language sql stable as $$
    select nullif(current_setting('ruum.tenant_id', true), '')::uuid
$$;

create function ruum_current_user() returns uuid
language sql stable as $$
    select nullif(current_setting('ruum.user_id', true), '')::uuid
$$;

create function ruum_is_system() returns boolean
language sql stable as $$
    select coalesce(current_setting('ruum.system', true), '') = 'on'
$$;

-- ---------------------------------------------------------------------------- tenants

create table tenants (
    id              uuid primary key,             -- this is the tenant_id
    name            text not null check (length(name) between 1 and 100),
    kind            tenant_kind not null,
    plan            plan not null default 'free',
    region          text not null default 'hel1',
    root_node_id    uuid,                         -- set in the same transaction as the root node
    -- Tenant policy: allowed sharing, identity providers, retention overrides.
    settings        jsonb not null default '{}'::jsonb,
    created_at      timestamptz not null default now(),
    created_kind    principal_kind not null,
    created_by      uuid not null,
    updated_at      timestamptz not null default now()
);

-- The journal sequence per tenant. One row per tenant; `ruum_next_seq` takes the next number in
-- the caller's transaction, so journal rows have no gaps and no duplicates.
create table journal_counters (
    tenant_id       uuid primary key references tenants (id),
    last_seq        bigint not null default 0
);

create function ruum_next_seq(t uuid) returns bigint
language sql volatile as $$
    update journal_counters set last_seq = last_seq + 1 where tenant_id = t returning last_seq
$$;

-- ---------------------------------------------------------------------------- identity

-- A person. `tenant_id` is the personal (home) tenant, made at first sign-in. A user can be a
-- member or guest of other tenants through `memberships`.
create table users (
    id              uuid primary key,
    tenant_id       uuid not null references tenants (id),
    email           text not null,
    display_name    text not null,
    avatar_url      text,
    created_at      timestamptz not null default now(),
    updated_at      timestamptz not null default now(),
    deleted_at      timestamptz
);
create unique index users_email_key on users (lower(email)) where deleted_at is null;

-- A linked identity at an identity provider (through Zitadel). Linked only by an explicit action
-- of the signed-in user, or by a verified email from a trusted provider.
create table identities (
    id              uuid primary key,
    tenant_id       uuid not null references tenants (id),   -- the user's home tenant
    user_id         uuid not null references users (id),
    issuer          text not null,
    subject         text not null,
    email           text,
    email_verified  boolean not null default false,
    linked_at       timestamptz not null default now(),
    unique (issuer, subject)
);
create index identities_user_idx on identities (user_id);

-- A web session. The cookie holds a random secret; the table holds its SHA-256.
create table sessions (
    id              uuid primary key,
    tenant_id       uuid not null references tenants (id),   -- the user's home tenant
    user_id         uuid not null references users (id),
    secret_hash     bytea not null unique,
    created_at      timestamptz not null default now(),
    expires_at      timestamptz not null,
    last_seen_at    timestamptz not null default now(),
    ended_at        timestamptz,
    user_agent      text,
    ip              inet
);
create index sessions_user_idx on sessions (user_id) where ended_at is null;

-- ---------------------------------------------------------------------------- members

create table memberships (
    tenant_id       uuid not null references tenants (id),
    user_id         uuid not null references users (id),
    role            member_role not null,
    joined_at       timestamptz not null default now(),
    created_kind    principal_kind not null,
    created_by      uuid not null,
    created_obo     uuid,
    updated_at      timestamptz not null default now(),
    primary key (tenant_id, user_id)
);
create index memberships_user_idx on memberships (user_id);

create table invitations (
    id              uuid primary key,
    tenant_id       uuid not null references tenants (id),
    email           text not null,
    role            member_role not null check (role <> 'owner'),
    token_hash      bytea not null unique,
    message         text,
    created_at      timestamptz not null default now(),
    created_kind    principal_kind not null,
    created_by      uuid not null,
    created_obo     uuid,
    expires_at      timestamptz not null,
    accepted_at     timestamptz,
    accepted_by     uuid references users (id),
    revoked_at      timestamptz
);
create index invitations_open_idx on invitations (tenant_id, created_at desc)
    where accepted_at is null and revoked_at is null;
create unique index invitations_open_email_key on invitations (tenant_id, lower(email))
    where accepted_at is null and revoked_at is null;

-- ---------------------------------------------------------------------------- nodes

create table nodes (
    id              uuid primary key,             -- UUIDv7, never changes
    tenant_id       uuid not null references tenants (id),
    parent_id       uuid,                         -- null only for the tenant root
    name            text not null check (length(name) between 1 and 255 and name not in ('.', '..')
                                         and position('/' in name) = 0),
    kind            node_kind not null,
    type            text,                         -- MIME type, 'album', 'project', 'database', ...
    link_kind       link_kind,
    link_target     uuid,
    head_version    uuid,
    owner_id        uuid not null references users (id),
    size_bytes      bigint not null default 0 check (size_bytes >= 0),
    revision        bigint not null default 1,
    -- Trash: `trashed_at` is set on the trashed node and on every node of its subtree;
    -- `trash_root` is the node the user trashed, so restore brings back the same subtree.
    trashed_at      timestamptz,
    trash_root      uuid,
    purge_after     timestamptz,
    trashed_kind    principal_kind,
    trashed_by      uuid,
    trashed_obo     uuid,
    created_at      timestamptz not null default now(),
    created_kind    principal_kind not null,
    created_by      uuid not null,
    created_obo     uuid,
    updated_at      timestamptz not null default now(),
    updated_kind    principal_kind not null,
    updated_by      uuid not null,
    updated_obo     uuid,
    unique (tenant_id, id),
    -- The parent is in the same tenant.
    foreign key (tenant_id, parent_id) references nodes (tenant_id, id),
    -- Only links have link fields. `link_target` has no foreign key: a link stays when its
    -- target is purged (a broken link, which the UI hides; section 2.3).
    check ((kind = 'link') = (link_kind is not null and link_target is not null)),
    check (kind <> 'link' or head_version is null),
    check ((trashed_at is null) = (trash_root is null))
);

-- A name is unique among the live (not trashed) children of a folder (section 2.6).
create unique index nodes_live_name_key on nodes (tenant_id, parent_id, name) where trashed_at is null;
-- One root per tenant.
create unique index nodes_root_key on nodes (tenant_id) where parent_id is null;
create index nodes_children_idx on nodes (tenant_id, parent_id, kind, name) where trashed_at is null;
create index nodes_trash_idx on nodes (tenant_id, trashed_at desc) where trashed_at is not null and id = trash_root;
create index nodes_link_target_idx on nodes (tenant_id, link_target) where link_target is not null;
-- Search by name (step 1): case-insensitive "contains".
create index nodes_name_trgm_idx on nodes using gin (lower(name) gin_trgm_ops) where trashed_at is null;

alter table tenants
    add foreign key (id, root_node_id) references nodes (tenant_id, id) deferrable initially deferred;

-- ---------------------------------------------------------------------------- blobs and versions

-- One version of a node: a complete manifest (snapshot model, section 5.3).
create table versions (
    id              uuid primary key,
    tenant_id       uuid not null references tenants (id),
    node_id         uuid not null,
    manifest        bytea not null,               -- encoded list of chunk hashes, or the typed-node manifest
    size_bytes      bigint not null check (size_bytes >= 0),
    content_hash    bytea not null check (length(content_hash) = 32),   -- BLAKE3 of the whole content
    content_type    text,
    restored_from   uuid,
    label           text check (label is null or length(label) between 1 and 200),
    actor_kind      principal_kind not null,
    actor           uuid not null,                -- user, app or agent that did the write
    on_behalf_of    uuid,                         -- the user, when the actor is an agent or an app
    created_at      timestamptz not null default now(),
    -- Coalescing (section 5.3) merges saves into the newest version; this is its last save.
    updated_at      timestamptz not null default now(),
    unique (tenant_id, id),
    foreign key (tenant_id, node_id) references nodes (tenant_id, id),
    foreign key (tenant_id, restored_from) references versions (tenant_id, id)
);
create index versions_node_idx on versions (tenant_id, node_id, created_at desc);
create index versions_actor_idx on versions (tenant_id, actor, created_at desc);

alter table nodes
    add foreign key (tenant_id, head_version) references versions (tenant_id, id) deferrable initially deferred;

-- Chunks stored in object storage at /<tenant>/<hash-prefix>/<hash>, once per tenant (no dedup
-- across tenants). Used for "which chunks are missing" and garbage collection.
create table chunks (
    tenant_id       uuid not null references tenants (id),
    hash            bytea not null check (length(hash) = 32),   -- BLAKE3 of the chunk
    size_bytes      integer not null check (size_bytes > 0),
    created_at      timestamptz not null default now(),
    -- Garbage collection (mark and sweep with a grace period) uses this.
    last_marked_at  timestamptz,
    primary key (tenant_id, hash)
);

-- Resumable upload sessions.
create table uploads (
    id              uuid primary key,
    tenant_id       uuid not null references tenants (id),
    -- Target: a new version of `node_id`, or a new file `name` in `parent_id`.
    node_id         uuid,
    base_version    uuid,
    parent_id       uuid,
    name            text,
    on_conflict     text not null default 'fail' check (on_conflict in ('fail', 'rename')),
    content_type    text,
    size_bytes      bigint not null check (size_bytes >= 0),
    content_hash    bytea not null check (length(content_hash) = 32),
    manifest        bytea not null,               -- the chunk list the client sent
    status          upload_status not null default 'open',
    result_version  uuid,
    created_at      timestamptz not null default now(),
    actor_kind      principal_kind not null,
    actor           uuid not null,
    on_behalf_of    uuid,
    expires_at      timestamptz not null,
    completed_at    timestamptz,
    unique (tenant_id, id),
    foreign key (tenant_id, node_id) references nodes (tenant_id, id),
    foreign key (tenant_id, parent_id) references nodes (tenant_id, id),
    check ((node_id is not null) <> (parent_id is not null and name is not null))
);
create index uploads_open_idx on uploads (tenant_id, expires_at) where status = 'open';

-- ---------------------------------------------------------------------------- tags

create table tags (
    id              uuid primary key,
    tenant_id       uuid not null references tenants (id),
    name            text not null check (length(name) between 1 and 64),
    color           text check (color is null or color ~ '^#[0-9a-fA-F]{6}$'),
    created_at      timestamptz not null default now(),
    created_kind    principal_kind not null,
    created_by      uuid not null,
    created_obo     uuid,
    updated_at      timestamptz not null default now(),
    unique (tenant_id, id)
);
create unique index tags_name_key on tags (tenant_id, lower(name));

create table node_tags (
    tenant_id       uuid not null references tenants (id),
    node_id         uuid not null,
    tag_id          uuid not null,
    created_at      timestamptz not null default now(),
    created_kind    principal_kind not null,
    created_by      uuid not null,
    created_obo     uuid,
    primary key (tenant_id, node_id, tag_id),
    foreign key (tenant_id, node_id) references nodes (tenant_id, id),
    foreign key (tenant_id, tag_id) references tags (tenant_id, id) on delete cascade
);
create index node_tags_tag_idx on node_tags (tenant_id, tag_id, node_id);

-- ---------------------------------------------------------------------------- agents

-- An agent principal. It belongs to one user and acts for that user (section 3.4).
create table agents (
    id                  uuid primary key,
    tenant_id           uuid not null references tenants (id),
    owner_id            uuid not null references users (id),
    name                text not null check (length(name) between 1 and 100),
    description         text,
    -- Scope: the effective right is the user's right intersected with this.
    scope_subtree       uuid,
    scope_max_role      grant_role not null,
    scope_expires_at    timestamptz,
    budget              jsonb not null default '{}'::jsonb,
    disabled_at         timestamptz,
    disabled_reason     agent_disabled_reason,
    created_at          timestamptz not null default now(),
    created_kind        principal_kind not null,
    created_by          uuid not null,
    created_obo         uuid,
    updated_at          timestamptz not null default now(),
    last_used_at        timestamptz,
    unique (tenant_id, id),
    foreign key (tenant_id, scope_subtree) references nodes (tenant_id, id),
    check ((disabled_at is null) = (disabled_reason is null))
);
create index agents_owner_idx on agents (tenant_id, owner_id);

-- API keys of agents. Only the SHA-256 of the key is stored. Keys start with `ruum_agent_`.
create table agent_keys (
    id              uuid primary key,
    tenant_id       uuid not null references tenants (id),
    agent_id        uuid not null,
    name            text,
    prefix          text not null check (prefix like 'ruum\_agent\_%'),
    key_hash        bytea not null unique check (length(key_hash) = 32),
    created_at      timestamptz not null default now(),
    created_kind    principal_kind not null,
    created_by      uuid not null,
    created_obo     uuid,
    expires_at      timestamptz not null,
    last_used_at    timestamptz,
    revoked_at      timestamptz,
    revoked_kind    principal_kind,
    revoked_by      uuid,
    foreign key (tenant_id, agent_id) references agents (tenant_id, id)
);
create index agent_keys_agent_idx on agent_keys (tenant_id, agent_id);

-- ---------------------------------------------------------------------------- sharing

-- Grants of a role on a node. The authz engine (SpiceDB) holds the same facts as relationships;
-- this table is the source to rebuild them and to list grants.
create table grants (
    id                  uuid primary key,
    tenant_id           uuid not null references tenants (id),
    node_id             uuid not null,
    grantee_kind        grantee_kind not null,
    grantee_user_id     uuid references users (id),
    grantee_email       text,
    role                grant_role not null,
    created_at          timestamptz not null default now(),
    created_kind        principal_kind not null,
    created_by          uuid not null,
    created_obo         uuid,
    foreign key (tenant_id, node_id) references nodes (tenant_id, id),
    check (
        (grantee_kind = 'user' and grantee_user_id is not null and grantee_email is null) or
        (grantee_kind = 'email' and grantee_user_id is null and grantee_email is not null) or
        (grantee_kind = 'tenant_members' and grantee_user_id is null and grantee_email is null)
    )
);
create unique index grants_user_key on grants (tenant_id, node_id, grantee_user_id) where grantee_kind = 'user';
create unique index grants_email_key on grants (tenant_id, node_id, lower(grantee_email)) where grantee_kind = 'email';
create unique index grants_members_key on grants (tenant_id, node_id) where grantee_kind = 'tenant_members';

-- Secret share links (`anyone_with_link`). Only hashes of the token and password are stored.
create table share_links (
    id              uuid primary key,
    tenant_id       uuid not null references tenants (id),
    node_id         uuid not null,
    role            grant_role not null check (role in ('view', 'comment', 'edit')),
    token_hash      bytea not null unique check (length(token_hash) = 32),
    password_hash   text,                         -- Argon2id PHC string
    expires_at      timestamptz,
    created_at      timestamptz not null default now(),
    created_kind    principal_kind not null,
    created_by      uuid not null,
    created_obo     uuid,
    revoked_at      timestamptz,
    revoked_kind    principal_kind,
    revoked_by      uuid,
    revoked_obo     uuid,
    foreign key (tenant_id, node_id) references nodes (tenant_id, id)
);
create index share_links_node_idx on share_links (tenant_id, node_id) where revoked_at is null;

-- "Shared with me": the roots of grants per principal (section 3.3), so the list is a query and
-- not a graph walk. `tenant_members` grants are expanded at read time from memberships.
create table share_index (
    tenant_id       uuid not null references tenants (id),
    principal_kind  principal_kind not null,
    principal_id    uuid not null,
    node_id         uuid not null,
    grant_id        uuid not null references grants (id) on delete cascade,
    role            grant_role not null,
    shared_at       timestamptz not null default now(),
    shared_kind     principal_kind not null,
    shared_by       uuid not null,
    shared_obo      uuid,
    primary key (tenant_id, principal_id, node_id),
    foreign key (tenant_id, node_id) references nodes (tenant_id, id)
);
create index share_index_principal_idx on share_index (principal_id, shared_at desc);

-- ---------------------------------------------------------------------------- journal and audit

-- The change journal (section 2.5). One row per change, written in the same transaction as the
-- change. `seq` grows per tenant (see ruum_next_seq). `data` follows contracts/events/.
create table journal (
    tenant_id       uuid not null references tenants (id),
    seq             bigint not null,
    node_id         uuid,
    type            text not null,
    actor_kind      principal_kind not null,
    actor           uuid not null,
    on_behalf_of    uuid,
    at              timestamptz not null default now(),
    data            jsonb not null default '{}'::jsonb,
    primary key (tenant_id, seq)
);
create index journal_node_idx on journal (tenant_id, node_id, seq);
create index journal_actor_idx on journal (tenant_id, actor, seq desc);

-- The audit log per tenant (section 15): auth, members, agents, keys, sharing, and node changes.
create table audit (
    id              uuid primary key,
    tenant_id       uuid not null references tenants (id),
    action          text not null,
    actor_kind      principal_kind not null,
    actor           uuid not null,
    on_behalf_of    uuid,
    target_type     text,
    target_id       text,
    ip              inet,
    user_agent      text,
    data            jsonb not null default '{}'::jsonb,
    at              timestamptz not null default now()
);
create index audit_tenant_at_idx on audit (tenant_id, at desc);
create index audit_actor_idx on audit (tenant_id, actor, at desc);

-- Idempotency keys of writes. A retry with the same key and request hash gets the stored
-- response for 24 hours. For calls without a tenant, `tenant_id` is the user's home tenant.
create table idempotency_keys (
    tenant_id       uuid not null references tenants (id),
    actor           uuid not null,
    key             text not null check (length(key) between 8 and 255),
    method          text not null,
    path            text not null,
    request_hash    bytea not null,
    status_code     smallint,                     -- null while the first request runs
    response_body   bytea,
    response_headers jsonb,
    created_at      timestamptz not null default now(),
    expires_at      timestamptz not null,
    primary key (tenant_id, actor, key)
);
create index idempotency_keys_expiry_idx on idempotency_keys (expires_at);

-- ---------------------------------------------------------------------------- row-level security

-- Tables with plain tenant isolation.
do $$
declare
    t text;
begin
    foreach t in array array[
        'journal_counters', 'identities', 'sessions', 'invitations', 'nodes', 'versions',
        'chunks', 'uploads', 'tags', 'node_tags', 'agents', 'agent_keys', 'grants',
        'share_links', 'journal', 'audit', 'idempotency_keys'
    ] loop
        execute format('alter table %I enable row level security', t);
        execute format('alter table %I force row level security', t);
        execute format(
            'create policy tenant_isolation on %I
                 using (tenant_id = ruum_current_tenant() or ruum_is_system())
                 with check (tenant_id = ruum_current_tenant() or ruum_is_system())', t);
    end loop;
end
$$;

-- Tenants: the row of the current tenant, and the tenants where the current user is a member
-- or guest (for the tenant list).
alter table tenants enable row level security;
alter table tenants force row level security;
create policy tenant_isolation on tenants
    using (id = ruum_current_tenant() or ruum_is_system()
           or exists (select 1 from memberships m
                      where m.tenant_id = tenants.id and m.user_id = ruum_current_user()))
    with check (id = ruum_current_tenant() or ruum_is_system());

-- Memberships: the rows of the current tenant, and the current user's own rows.
alter table memberships enable row level security;
alter table memberships force row level security;
create policy tenant_isolation on memberships
    using (tenant_id = ruum_current_tenant() or ruum_is_system() or user_id = ruum_current_user())
    with check (tenant_id = ruum_current_tenant() or ruum_is_system());

-- Users: the current user, users whose home is the current tenant, and members and guests of the
-- current tenant (to show names in member lists and grants).
alter table users enable row level security;
alter table users force row level security;
create policy tenant_isolation on users
    using (id = ruum_current_user() or tenant_id = ruum_current_tenant() or ruum_is_system()
           or exists (select 1 from memberships m
                      where m.user_id = users.id and m.tenant_id = ruum_current_tenant()))
    with check (tenant_id = ruum_current_tenant() or ruum_is_system());

-- Share index: the rows of the current tenant, and the rows for the current user in any tenant
-- ("Shared with me" spans tenants).
alter table share_index enable row level security;
alter table share_index force row level security;
create policy tenant_isolation on share_index
    using (tenant_id = ruum_current_tenant() or ruum_is_system() or principal_id = ruum_current_user())
    with check (tenant_id = ruum_current_tenant() or ruum_is_system());

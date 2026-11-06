-- Ruum step 2 (Databases): typed nodes and the provider contract, database metadata, column
-- classes, migrations, branches, restore points, time-limited grants for agents, approvals of
-- destructive statements, and the query log. See docs/DESIGN.md, sections 7.1, 8 and 17.3,
-- contracts/providers/, contracts/manifests/database.schema.json and docs/STEP2.md.
--
-- DRAFT for review (step 2 specify phase). The rules of 0001 hold for every table: `tenant_id`
-- on every row and in every query, forced row-level security, UUIDv7 IDs made by the
-- application, timestamptz in UTC, and the principal pair `*_kind` / `*_by` / `*_obo` on writes.
--
-- This migration holds METADATA only, in the platform database. The data of a database node
-- lives in the engine: a Postgres database in the shared data cluster, or a SQLite file
-- replicated to object storage. No password is stored anywhere (section 8.4).

-- ---------------------------------------------------------------------------- types

create type data_class as enum ('normal', 'personal', 'secret');
create type database_engine as enum ('postgres', 'sqlite');
-- The node status of the provider contract (`ProviderState`).
create type provider_state as enum (
    'provisioning', 'ready', 'updating', 'degraded', 'suspended', 'failed', 'deleted'
);
create type data_copy as enum ('masked', 'schema_only', 'full');
create type database_grant_kind as enum ('write_live', 'read_personal');
create type approval_status as enum ('pending', 'approved', 'denied', 'expired', 'used');
create type migration_status as enum ('applied', 'failed', 'rolled_back', 'dry_run');
create type restore_point_reason as enum ('manual', 'before_write_grant', 'before_migration', 'before_restore');
create type query_mode as enum ('read', 'write');
create type query_outcome as enum ('ok', 'error', 'timeout', 'cancelled', 'approval_required');
create type provider_hook as enum (
    'copied', 'moved', 'renamed', 'trashed', 'restored', 'purged', 'access_changed',
    'tenant_exported', 'tenant_deleted'
);

-- A manifest version of a typed node is a row of `versions` with this encoding: `manifest`
-- holds the canonical JSON of the manifest, `content_hash` its BLAKE3.
alter table versions drop constraint versions_encoding_check;
alter table versions add constraint versions_encoding_check
    check (encoding in ('chunks_v1', 'typed_manifest_v1'));

-- ---------------------------------------------------------------------------- typed nodes

-- One row per typed node (nodes.kind = 'typed'): which provider owns it, and the state of the
-- control loop (section 8.1). `desired_version` is the newest manifest version; the provider
-- reports `observed_version` when the real thing matches it. `generation` is the newest
-- status report (`ProviderStatus.generation`); older reports are ignored.
create table typed_nodes (
    tenant_id           uuid not null references tenants (id),
    node_id             uuid not null,
    provider            text not null check (provider ~ '^[a-z][a-z0-9_.-]{1,62}$'),   -- 'ruum.database'
    type                text not null check (type ~ '^[a-z][a-z0-9_.]{1,62}$'),        -- 'database'
    desired_version     uuid not null,
    observed_version    uuid,
    state               provider_state not null default 'provisioning',
    state_reason        text check (state_reason is null or state_reason ~ '^[a-z][a-z0-9_]{0,62}$'),
    state_message       text check (state_message is null or length(state_message) <= 1000),
    state_detail        jsonb not null default '{}'::jsonb,
    generation          bigint not null default 0,
    state_at            timestamptz not null default now(),
    -- The provider's own small state (not content), for example the engine-side name.
    provider_state      jsonb not null default '{}'::jsonb,
    created_at          timestamptz not null default now(),
    updated_at          timestamptz not null default now(),
    primary key (tenant_id, node_id),
    foreign key (tenant_id, node_id) references nodes (tenant_id, id),
    foreign key (tenant_id, desired_version) references versions (tenant_id, id),
    foreign key (tenant_id, observed_version) references versions (tenant_id, id)
);
create index typed_nodes_reconcile_idx on typed_nodes (state_at)
    where state in ('provisioning', 'updating', 'degraded') or observed_version is distinct from desired_version;

-- A node of kind `typed` has its row here (checked at commit, so the node and its row are
-- written in one transaction in any order).
create function ruum_check_typed_node() returns trigger
language plpgsql as $$
begin
    if new.kind = 'typed' and not exists (
        select 1 from typed_nodes t where t.tenant_id = new.tenant_id and t.node_id = new.id
    ) then
        raise exception 'typed node % has no typed_nodes row', new.id
            using errcode = 'foreign_key_violation';
    end if;
    return null;
end
$$;
create constraint trigger nodes_typed_row_check
    after insert on nodes deferrable initially deferred
    for each row when (new.kind = 'typed') execute function ruum_check_typed_node();

-- Lifecycle hook deliveries to providers (section 7.1): one row per journal entry that touches
-- a typed node, delivered in `seq` order per node, with retries. A first-party provider reads
-- the same queue in the server process. `delivery_id` is the `Ruum-Delivery-Id`.
create table provider_deliveries (
    tenant_id           uuid not null references tenants (id),
    delivery_id         uuid not null,
    provider            text not null,
    hook                provider_hook not null,
    node_id             uuid,                         -- null for the tenant hooks
    target_node_id      uuid,                         -- `copied`: the new node
    seq                 bigint,                       -- the journal entry, null for the tenant hooks
    payload             jsonb not null,               -- `HookDelivery`
    attempts            integer not null default 0,
    next_attempt_at     timestamptz not null default now(),
    delivered_at        timestamptz,
    last_error          text,
    created_at          timestamptz not null default now(),
    primary key (tenant_id, delivery_id),
    foreign key (tenant_id, seq) references journal (tenant_id, seq),
    check ((node_id is null) = (hook in ('tenant_exported', 'tenant_deleted')))
);
create index provider_deliveries_pending_idx on provider_deliveries (next_attempt_at)
    where delivered_at is null;
create index provider_deliveries_node_idx on provider_deliveries (tenant_id, node_id, seq)
    where delivered_at is null;

-- ---------------------------------------------------------------------------- databases

-- One row per database node (typed node of type 'database'). The manifest itself is the head
-- version; the columns below copy what the engine needs and what queries filter on.
create table databases (
    tenant_id           uuid not null references tenants (id),
    node_id             uuid not null,
    engine              database_engine not null,
    engine_version      text not null check (engine_version ~ '^[0-9]{1,3}$'),
    plan                text not null default 'shared-small' check (plan in ('shared-small')),
    -- Where the data lives. Postgres: the cluster ID (`hel1-shared-1`; on the laptop `dev`) and
    -- the database name in it (`db_<node id without dashes>`). SQLite: the storage ID and the
    -- object key prefix of the replica in object storage.
    cluster             text not null,
    engine_name         text not null check (engine_name ~ '^[a-z0-9_./-]{1,200}$'),
    -- A branch (section 8.4, decision 18): the source database and what data it took. A branch
    -- is a child node of its source (nodes.parent_id = branch_of).
    branch_of           uuid,
    branch_data         data_copy,
    branch_at           timestamptz,                  -- the source's point in time
    extensions          text[] not null default '{}',
    pitr_days           integer not null default 7 check (pitr_days = 7),
    size_bytes          bigint not null default 0 check (size_bytes >= 0),
    size_measured_at    timestamptz,
    created_at          timestamptz not null default now(),
    updated_at          timestamptz not null default now(),
    primary key (tenant_id, node_id),
    foreign key (tenant_id, node_id) references typed_nodes (tenant_id, node_id),
    foreign key (tenant_id, branch_of) references databases (tenant_id, node_id),
    unique (cluster, engine_name),
    check ((branch_of is null) = (branch_data is null)),
    check (branch_at is null or branch_of is not null),
    check (engine <> 'sqlite' or extensions = '{}')
);
create index databases_branch_idx on databases (tenant_id, branch_of) where branch_of is not null;

-- The engine roles of a Postgres database (section 8.4: no static passwords). The roles have
-- NOLOGIN; the platform connects as its own login role and uses `SET ROLE` per request, or
-- grants a short-lived login for a connection. One role per kind and database:
--   read          view, people          (read-only, sees personal values)
--   write         edit, people          (read-write)
--   owner         manage, migrations    (owns the schema)
--   masked_read   agents, read          (PostgreSQL Anonymizer masks personal and secret)
--   masked_write  agents, branch or write_live grant
--   secret_read   people with manage    (sees secret values)
create table database_roles (
    tenant_id           uuid not null references tenants (id),
    node_id             uuid not null,
    kind                text not null check (kind in ('read', 'write', 'owner', 'masked_read', 'masked_write', 'secret_read')),
    role_name           text not null check (role_name ~ '^[a-z][a-z0-9_]{1,62}$'),
    created_at          timestamptz not null default now(),
    primary key (tenant_id, node_id, kind),
    foreign key (tenant_id, node_id) references databases (tenant_id, node_id)
);
create unique index database_roles_name_key on database_roles (role_name);

-- The data class of each column (section 8.5). One row per column the engine has; the catalog
-- sync adds rows for new columns and marks dropped ones. The effective class is
-- `coalesce(confirmed_class, 'personal')`: an unconfirmed column fails safe. The newest
-- suggestion is kept here; every suggestion and confirmation is also in `audit`.
create table database_columns (
    tenant_id           uuid not null references tenants (id),
    node_id             uuid not null,
    schema_name         text not null check (length(schema_name) between 1 and 63),
    table_name          text not null check (length(table_name) between 1 and 63),
    column_name         text not null check (length(column_name) between 1 and 63),
    data_type           text not null,
    suggested_class     data_class,
    suggested_reason    text check (suggested_reason is null or length(suggested_reason) <= 500),
    suggested_kind      principal_kind,
    suggested_by        uuid,
    suggested_obo       uuid,
    suggested_at        timestamptz,
    confirmed_class     data_class,
    confirmed_by        uuid references users (id),   -- always a person (confirmColumnClasses is user-only)
    confirmed_at        timestamptz,
    -- The manifest version that holds the confirmation.
    confirmed_version   uuid,
    dropped_at          timestamptz,                  -- the column is gone from the engine
    first_seen_at       timestamptz not null default now(),
    primary key (tenant_id, node_id, schema_name, table_name, column_name),
    foreign key (tenant_id, node_id) references databases (tenant_id, node_id),
    foreign key (tenant_id, confirmed_version) references versions (tenant_id, id),
    check ((suggested_class is null) = (suggested_at is null)),
    check ((confirmed_class is null) = (confirmed_by is null and confirmed_at is null))
);
create index database_columns_unconfirmed_idx on database_columns (tenant_id, node_id)
    where confirmed_class is null and dropped_at is null;

-- Schema migrations (decision 19). A name runs once per database; `checksum` is BLAKE3 of `up`.
create table database_migrations (
    id                  uuid primary key,
    tenant_id           uuid not null references tenants (id),
    node_id             uuid not null,
    name                text not null check (name ~ '^[0-9]{4}_[a-z0-9_]{1,100}$'),
    checksum            bytea not null check (length(checksum) = 32),
    up_sql              text not null,
    -- The author's classes for new columns, by `<schema>.<table>.<column>` (suggestions).
    data_classes        jsonb not null default '{}'::jsonb,
    status              migration_status not null,
    error               text,
    restore_point_id    uuid,
    actor_kind          principal_kind not null,
    actor               uuid not null,
    on_behalf_of        uuid,
    at                  timestamptz not null default now(),
    unique (tenant_id, id),
    foreign key (tenant_id, node_id) references databases (tenant_id, node_id)
);
-- A name is applied once; failed and dry runs may repeat.
create unique index database_migrations_applied_key on database_migrations (tenant_id, node_id, name)
    where status = 'applied';
create index database_migrations_node_idx on database_migrations (tenant_id, node_id, at desc);

-- Restore points inside the backup window (decision 17). A point is a time (and on Postgres the
-- WAL position), not a copy. Automatic points come before each write grant, migration and
-- in-place restore.
create table database_restore_points (
    id                  uuid primary key,
    tenant_id           uuid not null references tenants (id),
    node_id             uuid not null,
    label               text check (label is null or length(label) between 1 and 200),
    reason              restore_point_reason not null,
    at                  timestamptz not null,
    wal_lsn             pg_lsn,                       -- Postgres only
    actor_kind          principal_kind not null,
    actor               uuid not null,
    on_behalf_of        uuid,
    created_at          timestamptz not null default now(),
    unique (tenant_id, id),
    foreign key (tenant_id, node_id) references databases (tenant_id, node_id),
    check (reason <> 'manual' or label is not null)
);
create index database_restore_points_node_idx on database_restore_points (tenant_id, node_id, at desc);

alter table database_migrations
    add foreign key (tenant_id, restore_point_id) references database_restore_points (tenant_id, id);

-- Restores (point in time, into a new node or in place). The job row; the result is the
-- target's status.
create table database_restores (
    id                  uuid primary key,
    tenant_id           uuid not null references tenants (id),
    node_id             uuid not null,                -- the source database
    target_node_id      uuid not null,                -- = node_id for in place
    in_place            boolean not null,
    restore_to          timestamptz not null,
    restore_point_id    uuid,                         -- the point the person picked, if any
    before_point_id     uuid,                         -- in place: the point of the state before
    state               provider_state not null default 'provisioning',
    error               text,
    actor_kind          principal_kind not null,
    actor               uuid not null,
    on_behalf_of        uuid,
    created_at          timestamptz not null default now(),
    finished_at         timestamptz,
    unique (tenant_id, id),
    foreign key (tenant_id, node_id) references databases (tenant_id, node_id),
    foreign key (tenant_id, target_node_id) references typed_nodes (tenant_id, node_id),
    foreign key (tenant_id, restore_point_id) references database_restore_points (tenant_id, id),
    foreign key (tenant_id, before_point_id) references database_restore_points (tenant_id, id),
    check (in_place = (node_id = target_node_id)),
    check (not in_place or before_point_id is not null)
);

-- ---------------------------------------------------------------------------- agents and data

-- Time-limited grants of an agent on a database (section 8.5): writes to the live database, or
-- real personal values. Always given by a person (the user the agent acts for). The SpiceDB
-- tuples `live_writer` / `personal_reader` follow the live rows; the core also checks
-- `expires_at` and `revoked_at` on every query.
create table database_access_grants (
    id                  uuid primary key,
    tenant_id           uuid not null references tenants (id),
    node_id             uuid not null,
    kind                database_grant_kind not null,
    agent_id            uuid not null,
    granted_by          uuid not null references users (id),
    reason              text check (reason is null or length(reason) <= 500),
    starts_at           timestamptz not null default now(),
    expires_at          timestamptz not null,
    revoked_at          timestamptz,
    revoked_kind        principal_kind,
    revoked_by          uuid,
    revoked_obo         uuid,
    restore_point_id    uuid,
    created_at          timestamptz not null default now(),
    unique (tenant_id, id),
    foreign key (tenant_id, node_id) references databases (tenant_id, node_id),
    foreign key (tenant_id, agent_id) references agents (tenant_id, id),
    foreign key (tenant_id, restore_point_id) references database_restore_points (tenant_id, id),
    check (expires_at > starts_at),
    check (expires_at <= starts_at + case kind when 'write_live' then interval '24 hours'
                                               else interval '8 hours' end),
    check ((kind = 'write_live') = (restore_point_id is not null)),
    check ((revoked_at is null) = (revoked_by is null))
);
create index database_access_grants_live_idx on database_access_grants (tenant_id, node_id, agent_id, kind)
    where revoked_at is null;
create index database_access_grants_expiry_idx on database_access_grants (expires_at)
    where revoked_at is null;

-- Destructive statements of agents that wait for a person (section 8.5). `statement_hash` is
-- BLAKE3 of the statement and the canonical JSON of the parameters, so an approval allows
-- exactly one statement with exactly those values, once. `params` holds the parameters as
-- markers (hidden values and pseudonyms), never raw personal or secret values; the raw values
-- are not needed, because the agent sends the statement again.
create table database_approvals (
    id                  uuid primary key,
    tenant_id           uuid not null references tenants (id),
    node_id             uuid not null,
    status              approval_status not null default 'pending',
    statement           text not null,
    statement_hash      bytea not null check (length(statement_hash) = 32),
    params              jsonb not null default '[]'::jsonb,
    reasons             text[] not null check (cardinality(reasons) > 0),
    estimated_rows      bigint,
    requested_kind      principal_kind not null,
    requested_by        uuid not null,
    requested_obo       uuid,
    decided_by          uuid references users (id),
    decided_at          timestamptz,
    comment             text check (comment is null or length(comment) <= 500),
    expires_at          timestamptz not null,
    used_at             timestamptz,
    created_at          timestamptz not null default now(),
    unique (tenant_id, id),
    foreign key (tenant_id, node_id) references databases (tenant_id, node_id),
    check ((status in ('approved', 'denied', 'used')) = (decided_by is not null)),
    check ((status = 'used') = (used_at is not null))
);
create index database_approvals_pending_idx on database_approvals (tenant_id, node_id, created_at desc)
    where status = 'pending';

-- The query log of the HTTP query API (sections 3.4, 8.5 and 11.2): every query with actor and
-- on_behalf_of, the statement as sent and the parameters as pseudonyms, never raw values.
-- High volume: partition by month and keep by the audit retention (open question in STEP2.md).
create table database_queries (
    id                  uuid primary key,
    tenant_id           uuid not null references tenants (id),
    node_id             uuid not null,
    mode                query_mode not null,
    statement           text not null,
    statement_hash      bytea not null check (length(statement_hash) = 32),
    params              jsonb not null default '[]'::jsonb,
    outcome             query_outcome not null,
    error_code          text,
    sqlstate            text check (sqlstate is null or sqlstate ~ '^[0-9A-Z]{5}$'),
    row_count           integer,
    rows_affected       bigint,
    truncated           boolean not null default false,
    elapsed_ms          integer,
    grant_id            uuid,
    approval_id         uuid,
    actor_kind          principal_kind not null,
    actor               uuid not null,
    on_behalf_of        uuid,
    at                  timestamptz not null default now(),
    unique (tenant_id, id),
    foreign key (tenant_id, node_id) references databases (tenant_id, node_id),
    foreign key (tenant_id, grant_id) references database_access_grants (tenant_id, id),
    foreign key (tenant_id, approval_id) references database_approvals (tenant_id, id)
);
create index database_queries_node_idx on database_queries (tenant_id, node_id, at desc);
create index database_queries_actor_idx on database_queries (tenant_id, actor, at desc);

-- ---------------------------------------------------------------------------- row-level security

do $$
declare
    t text;
begin
    foreach t in array array[
        'typed_nodes', 'provider_deliveries', 'databases', 'database_roles', 'database_columns',
        'database_migrations', 'database_restore_points', 'database_restores',
        'database_access_grants', 'database_approvals', 'database_queries'
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

-- Ruum step 3 (Web apps): web app nodes and their deployments, bindings, app roles, secrets
-- (names only), custom domains, partner API keys, approvals of agents' publishes, telemetry
-- classes, issues and reports, template setups and public projects. See docs/DESIGN.md, sections
-- 2.7, 9 and 17.4, contracts/manifests/app.schema.json and docs/STEP3.md.
--
-- DRAFT for review (step 3 specify phase). The rules of 0001 hold for every table: `tenant_id`
-- on every row and in every query, forced row-level security, UUIDv7 IDs made by the
-- application, timestamptz in UTC, and the principal pair `*_kind` / `*_by` / `*_obo` on writes.
--
-- This migration holds METADATA only. Archives are blobs (chunks of the blob store); secret
-- values live only in OpenBao (`app_secrets` has names, never values); logs, metrics, traces and
-- error events live in the telemetry store (Loki, Mimir, Tempo; one storage tenant per Ruum
-- tenant), never in the platform database.

-- ---------------------------------------------------------------------------- types

create type app_environment as enum ('live', 'preview');
create type app_deployment_state as enum (
    'pending', 'running', 'ready', 'live', 'failed', 'superseded', 'expired', 'rolled_back'
);
create type app_binding_kind as enum ('node', 'database_branch', 'api');
create type app_secret_kind as enum ('generated', 'entered');
create type app_domain_state as enum ('pending_dns', 'pending_certificate', 'active', 'failed');
create type telemetry_field_kind as enum ('log_field', 'metric_label', 'span_attribute');
create type issue_status as enum ('open', 'in_progress', 'closed', 'regressed');
create type issue_source as enum ('person', 'agent', 'monitoring', 'report');
create type publication_audience as enum ('private', 'signed_in', 'public');

-- The bearer of an issue receipt token (decision 0006, open question 24): the actor of
-- `issue.reveal_decided`. Like `share_link`, it is not a member; the ID is the issue node of the
-- report. `PrincipalKind` is an open enum (contracts/README.md).
alter type principal_kind add value if not exists 'issue_receipt';

-- A live deployment of a web app is a version of its node with this encoding: `manifest` holds
-- the canonical JSON `{ "deployment_id", "archive": { "content_hash", "size", "chunks": [...] },
-- "app_manifest": {...} }`, so the blob GC finds the chunks of every release (decision 0006-2).
alter table versions drop constraint versions_encoding_check;
alter table versions add constraint versions_encoding_check
    check (encoding in ('chunks_v1', 'typed_manifest_v1', 'app_release_v1'));

-- ---------------------------------------------------------------------------- tenants

-- Per tenant: the slug of the apps domain (`<app>.<slug>.ruumapps.net`, fixed once the first app
-- is deployed; 2 to 40 characters, no `--`, no `xn--` prefix, no `-` at either end, never one of
-- the reserved slugs of docs/STEP3.md, never a slug that another tenant used: `retired_app_slugs`) and what a Ruum platform admin approved in the admin console (design 9.5 and 12.3,
-- decision 22). `*_approved_by` names the platform admin's identity in the admin console (not a
-- tenant principal).
create table tenant_app_settings (
    tenant_id                   uuid primary key references tenants (id),
    slug                        text not null check (slug ~ '^[a-z0-9][a-z0-9-]{0,38}[a-z0-9]$'
                                    and slug !~ '--' and slug !~ '^xn-'),
    public_apps_approved_at     timestamptz,
    public_apps_approved_by     text,
    custom_domains_approved_at  timestamptz,
    custom_domains_approved_by  text,
    created_at                  timestamptz not null default now(),
    updated_at                  timestamptz not null default now(),
    check ((public_apps_approved_at is null) = (public_apps_approved_by is null)),
    check ((custom_domains_approved_at is null) = (custom_domains_approved_by is null))
);
-- A slug names one tenant on the apps domain. The index spans tenants (host names are global);
-- a clash answers `409 conflict` without naming the other tenant.
create unique index tenant_app_settings_slug_idx on tenant_app_settings (slug);

-- ---------------------------------------------------------------------------- apps

-- One row per web app node (typed node of type 'webapp', provider 'ruum.webapp'). The slug is one
-- DNS label with room for the preview prefix (`<8-char short_id>--<slug>` is at most 63
-- characters): 2 to 53 characters, no `--` (it separates the preview prefix), no `-` at either
-- end, no `xn--` prefix (decision 0006, review R10 and RC-5).
create table apps (
    tenant_id           uuid not null references tenants (id),
    node_id             uuid not null,
    slug                text not null check (slug ~ '^[a-z0-9][a-z0-9-]{0,51}[a-z0-9]$'
                            and slug !~ '--' and slug !~ '^xn-'),
    -- `AppSettings` of openapi.yaml: access, telemetry_mode, raw_copy_days, report_widget,
    -- auto_rollback.
    settings            jsonb not null default '{}'::jsonb,
    access              text not null default 'platform' check (access in ('platform', 'public')),
    live_deployment_id  uuid,
    slug_fixed_at       timestamptz,
    created_at          timestamptz not null default now(),
    updated_at          timestamptz not null default now(),
    primary key (tenant_id, node_id),
    foreign key (tenant_id, node_id) references typed_nodes (tenant_id, node_id),
    unique (tenant_id, slug)
);

-- Slugs that were used once and are never given again (decision 0006, review R10): old links and
-- bookmarks must not reach a stranger's app. `kind` `tenant`: the tenant slug of the apps domain,
-- unique across tenants (a system lookup, like `tenant_app_settings_slug_idx`); the row survives
-- the tenant, so `tenant_id` has no foreign key and names the last owner. `kind` `app`: an app
-- slug inside one tenant, given again only to the same app node (a restore from the trash).
create table retired_app_slugs (
    tenant_id           uuid not null,
    kind                text not null check (kind in ('tenant', 'app')),
    slug                text not null check (slug ~ '^[a-z0-9][a-z0-9-]{0,51}[a-z0-9]$'),
    app_node_id         uuid,
    retired_at          timestamptz not null default now(),
    check ((kind = 'app') = (app_node_id is not null))
);
create unique index retired_app_slugs_tenant_idx on retired_app_slugs (slug) where kind = 'tenant';
create unique index retired_app_slugs_app_idx on retired_app_slugs (tenant_id, slug) where kind = 'app';

-- Every deployment: previews and live ones (design 9.1). A live deployment has `version_id`, the
-- version of the app node it wrote at publish. `short_id` is the preview host prefix
-- (`<short_id>--<app>.<tenant>.ruumapps.net`). `archive_chunks` is the chunk list of the archive
-- (the same format as `versions.manifest` of `chunks_v1`), so a preview's archive is kept from
-- the GC while the preview exists. `stages` and `check_result` are `AppDeployment.stages` and
-- `.check`; their messages can quote log lines and are shown only to callers with `edit`.
create table app_deployments (
    tenant_id           uuid not null references tenants (id),
    id                  uuid not null,
    app_node_id         uuid not null,
    environment         app_environment not null,
    state               app_deployment_state not null default 'pending',
    short_id            text not null check (short_id ~ '^[a-z0-9]{8}$'),
    version_id          uuid,
    label               text check (label is null or length(label) <= 200),
    draft               text check (draft is null or draft ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    source_kind         text not null default 'file' check (source_kind in ('file', 'folder', 'build')),
    source_node_id      uuid,
    source_version_id   uuid,
    -- `source_kind` `folder`: the file versions that the platform packed into the archive
    -- (`[{"node_id", "version_id", "path"}]`), so `AppDeployment.source` names the code exactly.
    source_files        jsonb,
    archive_hash        bytea not null check (length(archive_hash) = 32),
    archive_size        bigint not null check (archive_size >= 0),
    archive_chunks      bytea not null,
    manifest            jsonb,
    stages              jsonb not null default '[]'::jsonb,
    check_result        jsonb,
    published_from      uuid,                         -- the preview a live deployment came from
    approval_id         uuid,                         -- the approval an agent's publish used
    created_kind        principal_kind not null,
    created_by          uuid not null,
    created_obo         uuid,
    created_at          timestamptz not null default now(),
    published_at        timestamptz,
    last_request_at     timestamptz,
    expires_at          timestamptz,                  -- previews only
    primary key (tenant_id, id),
    foreign key (tenant_id, app_node_id) references apps (tenant_id, node_id),
    foreign key (tenant_id, version_id) references versions (tenant_id, id),
    check ((environment = 'live') = (version_id is not null) or state in ('pending', 'running', 'failed')),
    check (environment = 'preview' or expires_at is null),
    check ((source_kind = 'folder') = (source_files is not null))
);
create unique index app_deployments_short_id_idx on app_deployments (tenant_id, short_id);
create index app_deployments_app_idx on app_deployments (tenant_id, app_node_id, created_at desc);
create index app_deployments_expiry_idx on app_deployments (expires_at)
    where environment = 'preview' and state = 'ready';
alter table apps add foreign key (tenant_id, live_deployment_id) references app_deployments (tenant_id, id)
    deferrable initially deferred;

-- The grants that bindings give (design 9.4, 9.10). `principal_id` is the app principal: the
-- app node ID for a live deployment, the deployment ID for a preview. A row is live while
-- `revoked_at` is null, and implies its tuple (`node#viewer|editor@app:<principal>` or
-- `app_api_scope:<target>/<api>/<scope>#consumer@app:<principal>`). `granted_*` is the deployer,
-- who had to hold the role or the right to grant the scopes (decision 0006-3).
create table app_bindings (
    tenant_id           uuid not null references tenants (id),
    id                  uuid not null,
    app_node_id         uuid not null,
    deployment_id       uuid not null,
    principal_id        uuid not null,
    name                text not null check (name ~ '^[a-z][a-z0-9_]{0,31}$'),
    kind                app_binding_kind not null,
    target_node_id      uuid not null,
    role                text check (role is null or role in ('view', 'edit')),
    api                 text check (api is null or api ~ '^[a-z][a-z0-9_]{0,31}$'),
    scopes              text[] not null default '{}',
    on_behalf_of_user   boolean not null default false,
    granted_kind        principal_kind not null,
    granted_by          uuid not null,
    granted_obo         uuid,
    created_at          timestamptz not null default now(),
    revoked_at          timestamptz,
    revoke_reason       text check (revoke_reason is null or revoke_reason in ('dropped', 'preview_expired', 'app_trashed', 'target_gone')),
    primary key (tenant_id, id),
    foreign key (tenant_id, deployment_id) references app_deployments (tenant_id, id),
    foreign key (tenant_id, target_node_id) references nodes (tenant_id, id),
    check ((kind = 'api') = (api is not null)),
    check ((kind = 'api') = (role is null)),
    check (kind <> 'api' or cardinality(scopes) > 0),
    check ((revoked_at is null) = (revoke_reason is null))
);
create index app_bindings_live_idx on app_bindings (tenant_id, principal_id) where revoked_at is null;
create index app_bindings_target_idx on app_bindings (tenant_id, target_node_id) where revoked_at is null;

-- App role grants (design 9.3): `app_role:<app>/<role>#holder@<grantee>`. A role that the live
-- manifest no longer declares keeps its grants (they give nothing until a version declares the
-- role again).
create table app_role_grants (
    tenant_id           uuid not null references tenants (id),
    id                  uuid not null,
    app_node_id         uuid not null,
    role                text not null check (role ~ '^[a-z][a-z0-9_]{0,31}$'),
    grantee_kind        text not null check (grantee_kind in ('user', 'group', 'tenant_members', 'email')),
    grantee_id          uuid,
    grantee_email       text,
    granted_kind        principal_kind not null,
    granted_by          uuid not null,
    granted_obo         uuid,
    created_at          timestamptz not null default now(),
    primary key (tenant_id, id),
    foreign key (tenant_id, app_node_id) references apps (tenant_id, node_id),
    check ((grantee_kind = 'email') = (grantee_email is not null)),
    check ((grantee_kind in ('user', 'group')) = (grantee_id is not null))
);
create unique index app_role_grants_unique_idx on app_role_grants
    (tenant_id, app_node_id, role, grantee_kind, coalesce(grantee_id::text, grantee_email, ''));

-- Secrets of an app: names and state only (design 9.9). The value is in OpenBao at a path that
-- the platform derives from tenant, app and name; `vault_version` is the KV version the running
-- app reads. Nothing here can rebuild a value.
create table app_secrets (
    tenant_id           uuid not null references tenants (id),
    app_node_id         uuid not null,
    name                text not null check (name ~ '^[A-Z][A-Z0-9_]{0,63}$' and name !~ '^(RUUM_|PORT$)'),
    kind                app_secret_kind not null,
    -- Previews get the live value of this entered secret. Stored state only: a user with `manage`
    -- sets it (`setAppSecret`); the manifest cannot (decision 0006-5, review R4).
    preview             boolean not null default false,
    preview_set_by      uuid references users (id),
    generator           text check (generator is null or generator in
                            ('random-16-bytes', 'random-32-bytes', 'random-64-bytes', 'ed25519-keypair', 'rsa-3072-keypair')),
    rotate_days         integer check (rotate_days is null or rotate_days between 1 and 9999),
    overlap_hours       integer check (overlap_hours is null or overlap_hours between 1 and 8760),
    vault_version       integer,
    set_at              timestamptz,
    rotated_at          timestamptz,
    next_rotation_at    timestamptz,
    set_kind            principal_kind,
    set_by              uuid,
    set_obo             uuid,
    created_at          timestamptz not null default now(),
    primary key (tenant_id, app_node_id, name),
    foreign key (tenant_id, app_node_id) references apps (tenant_id, node_id),
    check ((kind = 'generated') = (generator is not null)),
    check (kind = 'generated' or rotate_days is null),
    check ((set_at is null) = (vault_version is null)),
    check (kind = 'entered' or not preview),
    check (not preview or preview_set_by is not null)
);
create index app_secrets_rotation_idx on app_secrets (next_rotation_at) where next_rotation_at is not null;

-- Custom domains (design 9.5; decision 22). Host names are global, but only a verified (`active`)
-- domain holds its host across tenants: a pending row is unique only inside its tenant and
-- expires after 7 days (`pending_expires_at`), so nobody can squat a host they cannot prove. The
-- TXT proof decides who gets the host; a clash answers `409 conflict` without naming the other
-- tenant (decision 0006, review R10).
create table app_domains (
    tenant_id           uuid not null references tenants (id),
    id                  uuid not null,
    app_node_id         uuid not null,
    host                text not null check (host ~ '^([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,63}$' and length(host) <= 253),
    state               app_domain_state not null default 'pending_dns',
    txt_token_hash      bytea not null check (length(txt_token_hash) = 32),
    error               text check (error is null or length(error) <= 500),
    certificate_expires_at timestamptz,
    pending_expires_at  timestamptz,
    created_kind        principal_kind not null,
    created_by          uuid not null,
    created_obo         uuid,
    created_at          timestamptz not null default now(),
    primary key (tenant_id, id),
    foreign key (tenant_id, app_node_id) references apps (tenant_id, node_id),
    check ((state in ('pending_dns', 'pending_certificate')) = (pending_expires_at is not null))
);
create unique index app_domains_host_idx on app_domains (host) where state = 'active';
create unique index app_domains_tenant_host_idx on app_domains (tenant_id, host);

-- Partner API keys for external APIs (design 9.10). Only the SHA-256 of the key is stored; keys
-- start with `ruum_appkey_` so secret scanners find leaks (a rule in .gitleaks.toml), and carry at
-- least 128 random bits after the prefix.
create table app_api_keys (
    tenant_id           uuid not null references tenants (id),
    id                  uuid not null,
    app_node_id         uuid not null,
    name                text not null check (length(name) between 1 and 100),
    prefix              text not null check (prefix ~ '^ruum_appkey_[A-Za-z0-9]{6}$'),
    key_hash            bytea not null check (length(key_hash) = 32),
    api                 text not null check (api ~ '^[a-z][a-z0-9_]{0,31}$'),
    scopes              text[] not null check (cardinality(scopes) > 0),
    rate_limit_per_minute integer not null check (rate_limit_per_minute between 1 and 6000),
    expires_at          timestamptz not null,
    last_used_at        timestamptz,
    created_by          uuid not null references users (id),
    created_at          timestamptz not null default now(),
    revoked_at          timestamptz,
    revoked_kind        principal_kind,
    revoked_by          uuid,
    revoked_obo         uuid,
    primary key (tenant_id, id),
    foreign key (tenant_id, app_node_id) references apps (tenant_id, node_id),
    check (expires_at <= created_at + interval '365 days'),
    check ((revoked_at is null) = (revoked_by is null))
);
create unique index app_api_keys_hash_idx on app_api_keys (key_hash);

-- An agent's publish waits for a person (design 11.1, decision 0006-4). An approval is used once,
-- within 15 minutes, for exactly `deployment_id`.
create table app_approvals (
    tenant_id           uuid not null references tenants (id),
    id                  uuid not null,
    app_node_id         uuid not null,
    deployment_id       uuid not null,
    status              approval_status not null default 'pending',
    changes             jsonb not null,               -- `AppApproval.changes` (names only)
    telemetry_fields    jsonb not null default '[]'::jsonb,
    -- The column classes the decider confirmed with the publish (`DecideAppApprovalRequest.classes`).
    classes             jsonb not null default '[]'::jsonb,
    -- What the approval is bound to (review S5): the archive hash, the parsed manifest's hash and
    -- the binding targets resolved at preview time. A publish that resolves anything else fails.
    bound_archive_hash  bytea not null check (length(bound_archive_hash) = 32),
    bound_manifest_hash bytea not null check (length(bound_manifest_hash) = 32),
    bound_bindings      jsonb not null default '[]'::jsonb,
    requested_kind      principal_kind not null,
    requested_by        uuid not null,
    requested_obo       uuid,
    decided_by          uuid references users (id),
    decided_at          timestamptz,
    comment             text check (comment is null or length(comment) <= 500),
    expires_at          timestamptz not null,
    used_at             timestamptz,
    created_at          timestamptz not null default now(),
    primary key (tenant_id, id),
    foreign key (tenant_id, deployment_id) references app_deployments (tenant_id, id),
    check ((status in ('approved', 'denied', 'used')) = (decided_by is not null)),
    check ((status = 'used') = (used_at is not null))
);
create index app_approvals_pending_idx on app_approvals (tenant_id, app_node_id, created_at desc)
    where status = 'pending';

-- ---------------------------------------------------------------------------- telemetry

-- The classes of telemetry fields per app (design 9.6). Rows come from `telemetry-privacy.yaml`
-- of each deployment and from fields that ingestion saw. A field without a confirmed class
-- counts as `personal`. Metric labels may only be `normal`.
create table telemetry_fields (
    tenant_id           uuid not null references tenants (id),
    app_node_id         uuid not null,
    field               text not null check (field ~ '^[A-Za-z_][A-Za-z0-9_.]{0,199}$'),
    kind                telemetry_field_kind not null,
    class               data_class not null default 'personal',
    confirmed_by        uuid references users (id),
    confirmed_at        timestamptz,
    suggested_class     data_class,
    suggestion_reason   text check (suggestion_reason is null or length(suggestion_reason) <= 500),
    first_seen_at       timestamptz,
    created_at          timestamptz not null default now(),
    primary key (tenant_id, app_node_id, field, kind),
    foreign key (tenant_id, app_node_id) references apps (tenant_id, node_id),
    check ((confirmed_at is null) = (confirmed_by is null)),
    check (kind <> 'metric_label' or class = 'normal')
);

-- The per-end-user keys of the encrypted raw copy (design 9.6): each end user's raw `personal`
-- values are encrypted under their own key, wrapped by the tenant key (`tenant_keys`, card Q-4).
-- Deleting an end user deletes the key (crypto-shredding): every stored raw value of theirs is
-- then unreadable. `end_user` is the pairwise ID of the app, never a platform user ID.
create table telemetry_end_user_keys (
    tenant_id           uuid not null references tenants (id),
    app_node_id         uuid not null,
    end_user            text not null check (end_user ~ '^[A-Za-z0-9_-]{1,64}$'),
    wrapped_key         bytea,
    created_at          timestamptz not null default now(),
    shredded_at         timestamptz,
    primary key (tenant_id, app_node_id, end_user),
    foreign key (tenant_id, app_node_id) references apps (tenant_id, node_id),
    check ((shredded_at is null) = (wrapped_key is not null))
);

-- ---------------------------------------------------------------------------- issues

-- One row per issue node (typed node of type 'issue', provider 'ruum.issue', design 9.7). The
-- content (title, fields, body, status, assignee) is the node's versions; this row copies what
-- boards sort and filter by, and holds the counts of error issues (provider state: a new
-- occurrence does not make a version).
create table issues (
    tenant_id           uuid not null references tenants (id),
    node_id             uuid not null,
    status              issue_status not null default 'open',
    source              issue_source not null,
    template_id         text not null check (length(template_id) <= 100),
    template_version    text not null check (length(template_version) <= 100),
    app_node_id         uuid,
    fingerprint         text check (fingerprint is null or fingerprint ~ '^[0-9a-f]{32}$'),
    parent_issue_id     uuid,
    deployment_id       uuid,
    assignee_kind       principal_kind,
    assignee_id         uuid,
    occurrences         bigint not null default 0,
    users               integer not null default 0,
    first_seen_at       timestamptz,
    last_seen_at        timestamptz,
    first_version_id    uuid,
    muted_until         timestamptz,
    closed_at           timestamptz,
    updated_at          timestamptz not null default now(),
    primary key (tenant_id, node_id),
    foreign key (tenant_id, node_id) references typed_nodes (tenant_id, node_id),
    foreign key (tenant_id, app_node_id) references apps (tenant_id, node_id),
    check ((assignee_kind is null) = (assignee_id is null)),
    check (assignee_kind is null or assignee_kind in ('user', 'agent')),
    check (source not in ('monitoring') or fingerprint is not null)
);
-- One error group per app: the first occurrence of a fingerprint creates the issue, every later
-- one links and counts (design 9.7).
create unique index issues_fingerprint_idx on issues (tenant_id, app_node_id, fingerprint)
    where source = 'monitoring';
create index issues_board_idx on issues (tenant_id, status, updated_at desc);

-- A report from the report widget (design 9.7): the reporter's choices per field and the hold on
-- the raw copies of the visit. `receipt_token_hash` is the SHA-256 of the receipt token (reporters
-- who are not members). `reporter_email` is set only when the reporter typed one for a reply, and
-- is deleted 30 days after the issue closes (`reporter_email_delete_at`; decision 0006, review R13).
-- `visit_id` is the visit that the gateway issued (never a value from the client). Report
-- attachments (screenshot, records, choices, revealed values) never leave the issue: no copy,
-- template or publication carries them (review R12).
create table issue_reports (
    tenant_id           uuid not null references tenants (id),
    issue_node_id       uuid not null,
    visit_id            text not null check (visit_id ~ '^[0-9a-f]{32}$'),
    reporter            text not null check (reporter ~ '^[A-Za-z0-9_-]{1,64}$'),
    reporter_member     boolean not null,
    reporter_email      text check (reporter_email is null or length(reporter_email) <= 320),
    reporter_email_delete_at timestamptz,
    choices             jsonb not null default '{}'::jsonb,
    context             jsonb not null default '{}'::jsonb,
    screenshot_node_id  uuid,
    records             integer not null default 0,
    raw_copy_hold_until timestamptz,
    receipt_token_hash  bytea check (receipt_token_hash is null or length(receipt_token_hash) = 32),
    created_at          timestamptz not null default now(),
    primary key (tenant_id, issue_node_id),
    foreign key (tenant_id, issue_node_id) references issues (tenant_id, node_id),
    check (raw_copy_hold_until is null or raw_copy_hold_until <= created_at + interval '30 days'),
    check (reporter_member or receipt_token_hash is not null)
);
create unique index issue_reports_receipt_idx on issue_reports (receipt_token_hash)
    where receipt_token_hash is not null;

-- Requests to reveal fields of a report (design 9.7, "asking again later"). `fields` maps each
-- field name to `pending`, `allowed` or `refused`; a refused field cannot be asked again.
create table issue_reveal_requests (
    tenant_id           uuid not null references tenants (id),
    id                  uuid not null,
    issue_node_id       uuid not null,
    fields              jsonb not null,
    reason              text not null check (length(reason) between 1 and 500),
    requested_by        uuid not null references users (id),
    created_at          timestamptz not null default now(),
    decided_at          timestamptz,
    primary key (tenant_id, id),
    foreign key (tenant_id, issue_node_id) references issue_reports (tenant_id, issue_node_id)
);

-- ---------------------------------------------------------------------------- templates and publication

-- The setup of a project from a template (design 2.7). Gallery templates are bundled with the
-- platform (`template_id` `gallery:<slug>`); tenant templates are template folders of the same
-- tenant (a template is never read across tenants).
create table template_setups (
    tenant_id           uuid not null references tenants (id),
    id                  uuid not null,
    template_id         text not null check (length(template_id) <= 100),
    template_version    text not null check (length(template_version) <= 100),
    project_node_id     uuid not null,
    state               text not null default 'running' check (state in ('running', 'done', 'failed')),
    steps               jsonb not null default '[]'::jsonb,
    open_placeholders   text[] not null default '{}',
    created_kind        principal_kind not null,
    created_by          uuid not null,
    created_obo         uuid,
    created_at          timestamptz not null default now(),
    finished_at         timestamptz,
    primary key (tenant_id, id),
    foreign key (tenant_id, project_node_id) references nodes (tenant_id, id)
);

-- What of a project is public (design 9.9). The rows imply `viewer` tuples (`user:*` or
-- `anonymous:*`) on the chosen items only. Waits for restricted folders (decision 8).
create table project_publications (
    tenant_id           uuid not null references tenants (id),
    project_node_id     uuid not null,
    audience            publication_audience not null default 'private',
    items               jsonb not null default '[]'::jsonb,
    updated_by          uuid references users (id),
    updated_at          timestamptz not null default now(),
    primary key (tenant_id, project_node_id),
    foreign key (tenant_id, project_node_id) references nodes (tenant_id, id)
);

-- ---------------------------------------------------------------------------- row-level security

do $$
declare
    t text;
begin
    foreach t in array array[
        'tenant_app_settings', 'apps', 'app_deployments', 'app_bindings', 'app_role_grants',
        'app_secrets', 'app_domains', 'app_api_keys', 'app_approvals', 'telemetry_fields',
        'telemetry_end_user_keys', 'issues', 'issue_reports', 'issue_reveal_requests',
        'template_setups', 'project_publications', 'retired_app_slugs'
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

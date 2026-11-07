-- The first use of a web session in a tenant (decision 0003-4, contract need C-14).
--
-- A sign-in writes `auth.login` only in the user's home tenant. When the session is used for the
-- first time in another tenant where the user is a member, admin or owner, the server writes one
-- `auth.login` row there (`data.via = session_first_use`) and this marker, in one transaction in
-- that tenant. Guests get no row and no marker: a host does not learn when a guest is active.
--
-- `session_id` is the `sessions.id` row ID (never derived from the cookie secret). There is no
-- foreign key: `sessions` rows live in the user's home tenant. The sweeper deletes a marker when
-- its session has ended or expired (`identity::sweep`).

create table session_tenants (
    tenant_id     uuid not null references tenants (id),
    session_id    uuid not null,
    first_used_at timestamptz not null default now(),
    primary key (tenant_id, session_id)
);
create index session_tenants_session_idx on session_tenants (session_id);

alter table session_tenants enable row level security;
alter table session_tenants force row level security;
create policy tenant_isolation on session_tenants
    using (tenant_id = ruum_current_tenant() or ruum_is_system())
    with check (tenant_id = ruum_current_tenant() or ruum_is_system());

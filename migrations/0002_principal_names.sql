-- The last known name of a principal in a tenant (decision 0003-5, contract need C-7).
--
-- Answers name every actor (`PrincipalRef.display_name`). Current members and guests take their
-- name from `users`, agents from `agents`. A person who left the tenant is no longer visible
-- through the RLS of `users` (decision 0001-3), so the tenant keeps a snapshot of the name. It
-- is written when the membership ends, in the same transaction. Nothing is backfilled: no
-- removed member has a name to keep today.
--
-- GDPR deletion (DESIGN 15): the deletion job (not built yet) deletes the user's rows here in
-- every tenant. Answers then carry no name.

create table principal_names (
    tenant_id      uuid not null references tenants (id),
    principal_id   uuid not null,
    principal_kind principal_kind not null,
    display_name   text not null,
    updated_at     timestamptz not null default now(),
    primary key (tenant_id, principal_id)
);

alter table principal_names enable row level security;
alter table principal_names force row level security;
create policy tenant_isolation on principal_names
    using (tenant_id = ruum_current_tenant() or ruum_is_system())
    with check (tenant_id = ruum_current_tenant() or ruum_is_system());

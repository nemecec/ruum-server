-- The progress of the purge of an app's static sites (backlog W2S-4; Kimi re-check of the
-- W2S-1 fix).
--
-- Expand only (design 12.7, decision 42): a new table. The previous release does not read
-- it; its purge hook keeps working as before.
--
-- `app_purge_progress`: the purge hook of static sites deletes the indexes of an app's
-- deployments in ID order, one page at a time, and stores here the ID of the last deployment
-- whose index is gone. A delivery that stops at its time budget answers `503` after it stored
-- this cursor, and the next delivery goes on after it. The row goes with the app's rows
-- (`remove_app`).
--
-- The index of the keyset pages is migration 0015: it is built with `CONCURRENTLY`, which
-- cannot run in the transaction of this migration (Kimi review of W2S-4, finding 2).

create table app_purge_progress (
    tenant_id           uuid not null references tenants (id),
    app_node_id         uuid not null,
    static_after        uuid not null,
    updated_at          timestamptz not null default now(),
    primary key (tenant_id, app_node_id)
);

do $$
declare
    t text;
begin
    foreach t in array array['app_purge_progress'] loop
        execute format('alter table %I enable row level security', t);
        execute format('alter table %I force row level security', t);
        execute format(
            'create policy tenant_isolation on %I
                 using (tenant_id = ruum_current_tenant() or ruum_is_system())
                 with check (tenant_id = ruum_current_tenant() or ruum_is_system())', t);
    end loop;
end
$$;

-- The shared launch authority of the Kubernetes runtime driver (backlog I620-9; docs/STEP3.md
-- "The sandbox supervisor" 4, "Rollback").
--
-- Expand only (design 12.7, decision 42): two new tables. The previous release does not read
-- them; its driver keeps its per-process marks (and the driver is off on staging, decision
-- 0008-5).
--
-- A holder of the runtime ServiceAccount's token can put an older sealed record back into its
-- ConfigMap. The seal's generation and its list of launched pods go back with it, so the
-- server keeps both here, where that writer cannot reach, shared by every server replica:
--
-- `runtime_seal_marks`: the highest seal generation any server process read or wrote for a
-- record. The record is the instance and its ConfigMap's `metadata.uid` (a record deleted and
-- made again under the same name is another record). A seal below the mark is refused.
--
-- `runtime_launches`: the pods that took their launch data, one row per pod uid. A launch is
-- claimed here (insert once) before the answer leaves, so no replica answers a pod twice.
--
-- The driver deletes a record's rows when it deletes the record (the sweep). The rows hold no
-- value: ids, a generation and times.

create table runtime_seal_marks (
    tenant_id           uuid not null references tenants (id),
    instance_id         uuid not null,
    record_uid          text not null check (length(record_uid) between 1 and 128),
    generation          bigint not null check (generation >= 0),
    updated_at          timestamptz not null default now(),
    primary key (tenant_id, instance_id, record_uid)
);

create table runtime_launches (
    tenant_id           uuid not null references tenants (id),
    instance_id         uuid not null,
    record_uid          text not null check (length(record_uid) between 1 and 128),
    pod_uid             text not null check (length(pod_uid) between 1 and 128),
    launched_at         timestamptz not null default now(),
    primary key (tenant_id, instance_id, record_uid, pod_uid)
);

do $$
declare
    t text;
begin
    foreach t in array array['runtime_seal_marks', 'runtime_launches'] loop
        execute format('alter table %I enable row level security', t);
        execute format('alter table %I force row level security', t);
        execute format(
            'create policy tenant_isolation on %I
                 using (tenant_id = ruum_current_tenant() or ruum_is_system())
                 with check (tenant_id = ruum_current_tenant() or ruum_is_system())', t);
    end loop;
end
$$;

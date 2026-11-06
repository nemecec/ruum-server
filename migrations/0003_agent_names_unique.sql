-- Agent names are unique per owner and tenant, ignoring case (decision 0003-5).
--
-- Agent names come from OAuth dynamic registration (`client_name`), so the client picks them.
-- Two agents of one owner with the same name cannot be told apart in the activity log. On a
-- clash the server adds " (2)", " (3)", ... to the new name. This migration first renames the
-- duplicates that exist: the oldest agent keeps its name, the others are numbered by
-- `created_at`. The base name is cut so that the result fits the 100-character check.

-- `agents` forces row-level security; the migration works on all tenants.
select set_config('ruum.system', 'on', true);

do $$
declare
    r record;
    n integer;
    suffix text;
    candidate text;
begin
    for r in
        select id, tenant_id, owner_id, name
        from (
            select a.id, a.tenant_id, a.owner_id, a.name, a.created_at,
                   row_number() over (partition by a.tenant_id, a.owner_id, lower(a.name)
                                      order by a.created_at, a.id) as rn
            from agents a
        ) x
        where x.rn > 1
        order by x.created_at, x.id
    loop
        n := 2;
        loop
            suffix := ' (' || n || ')';
            candidate := left(r.name, 100 - length(suffix)) || suffix;
            exit when not exists (
                select 1 from agents
                where tenant_id = r.tenant_id and owner_id = r.owner_id
                  and lower(name) = lower(candidate));
            n := n + 1;
        end loop;
        update agents set name = candidate, updated_at = now() where id = r.id;
    end loop;
end
$$;

create unique index agents_owner_name_key on agents (tenant_id, owner_id, lower(name));

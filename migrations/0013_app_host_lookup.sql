-- The app gateway (card G-1a; docs/STEP3.md "System lookups" and "How an app runs" rule 3;
-- contracts/apps/gateway.openapi.yaml; decision 0006-1).
--
-- Expand only (design 12.7, decision 42): a new function and two new tables. The previous
-- release calls none of them.
--
-- 1. `ruum_app_host_lookup`: the narrow system lookup "host to app and deployment". The gateway
--    holds no general system access: its database role may only execute this function. The
--    function runs as its owner (`security definer`) with the system flag set for its own body
--    only (`ruum.system`, put back to the caller's value before it returns), a fixed
--    `search_path`, and
--    returns only the IDs `(tenant_id, app_id, deployment_id)` of the live deployment of the app
--    (no short ID) or of that ready preview. Nothing for an unknown tenant or app, an app in the
--    trash, a reserved slug, a retired slug (it is no longer in `apps` or `tenant_app_settings`),
--    a preview of another app, an expired preview or a deployment that is not serving.
--    `gateway.openapi.yaml` ("The check of every request" 1) names the app among the returned
--    values; the app ID is the node ID that the user token's audience and the session need.
-- 2. `app_sign_in_codes`: the one-time codes of the sign-in on an app origin (the platform
--    issues one to a signed-in user; the gateway redeems it once, within 60 seconds, through the
--    core's internal listener). Only the SHA-256 of a code is stored.
-- 3. `app_sessions`: the sessions of app origins. The cookie `__Host-ruum_app` holds a random
--    secret; the table holds its SHA-256. A session is bound to the app node, the environment
--    and, for a preview, the deployment. It ends with the platform session it came from.

create function ruum_app_host_lookup(p_tenant_slug text, p_app_slug text, p_short_id text default null)
returns table (tenant_id uuid, app_id uuid, deployment_id uuid)
language plpgsql
volatile
security definer
set search_path = pg_catalog, pg_temp
as $$
declare
    -- The system flag of the caller, put back before the function returns (also on an error:
    -- the exception block is a subtransaction, which undoes the change).
    v_before text := current_setting('ruum.system', true);
begin
    perform set_config('ruum.system', 'on', true);
    return query
        select s.tenant_id, a.node_id, d.id
        from public.tenant_app_settings s
        join public.apps a on a.tenant_id = s.tenant_id and a.slug = p_app_slug
        join public.nodes n on n.tenant_id = a.tenant_id and n.id = a.node_id
            and n.trashed_at is null
        join public.app_deployments d on d.tenant_id = a.tenant_id and d.app_node_id = a.node_id
        where s.slug = p_tenant_slug
          -- The reserved slugs of docs/STEP3.md "Host names and slugs" (tenant and app).
          and not (p_tenant_slug = any (array['content', 'www', 'api', 'admin', 'static',
                   'internal', 'ruum', 'status', 'mail', 'report', 'preview', 'app', 'apps',
                   'auth', 'login', 'id', 'cdn', 'assets', 'docs', 'help', 'support', 'blog',
                   'dev', 'staging', 'test', 'mta-sts', 'autoconfig'])
                   or length(p_tenant_slug) < 3 or p_tenant_slug like '\_%')
          and not (p_app_slug = any (array['content', 'www', 'api', 'admin', 'static',
                   'internal', 'ruum', 'status', 'mail', 'report', 'preview', 'app', 'apps',
                   'auth', 'login', 'id', 'cdn', 'assets', 'docs', 'help', 'support', 'blog',
                   'dev', 'staging', 'test', 'mta-sts', 'autoconfig'])
                   or length(p_app_slug) < 3 or p_app_slug like '\_%')
          and case
              when p_short_id is null then
                  d.id = a.live_deployment_id and d.environment = 'live' and d.state = 'live'
              else
                  d.short_id = p_short_id and d.environment = 'preview' and d.state = 'ready'
                  and (d.expires_at is null or d.expires_at > now())
              end;
    perform set_config('ruum.system', coalesce(v_before, ''), true);
exception when others then
    perform set_config('ruum.system', coalesce(v_before, ''), true);
    raise;
end
$$;

revoke all on function ruum_app_host_lookup(text, text, text) from public;

-- The gateway's database role. A migration runner without CREATEROLE skips the create; the
-- deployment then makes the role (with LOGIN) and grants EXECUTE itself. The role gets nothing
-- else: no table, no sequence, no other function.
do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'ruum_gateway') then
        begin
            create role ruum_gateway nologin;
        exception when insufficient_privilege or duplicate_object then
            null;
        end;
    end if;
    if exists (select 1 from pg_roles where rolname = 'ruum_gateway') then
        execute 'grant execute on function ruum_app_host_lookup(text, text, text) to ruum_gateway';
    end if;
end
$$;

-- ---------------------------------------------------------------------------- sign-in codes

create table app_sign_in_codes (
    tenant_id           uuid not null references tenants (id),
    code_hash           bytea not null check (length(code_hash) = 32),
    user_id             uuid not null references users (id),
    -- The platform session that asked for the code; the app session ends with it.
    platform_session_id uuid not null,
    app_node_id         uuid not null,
    environment         app_environment not null,
    -- The preview the code is for; null for the live app.
    deployment_id       uuid,
    -- SHA-256 of the gateway's state of this sign-in.
    state_hash          bytea not null check (length(state_hash) = 32),
    created_at          timestamptz not null default now(),
    expires_at          timestamptz not null,
    used_at             timestamptz,
    primary key (tenant_id, code_hash),
    check ((environment = 'preview') = (deployment_id is not null))
);
create index app_sign_in_codes_expiry_idx on app_sign_in_codes (expires_at);

-- ---------------------------------------------------------------------------- app sessions

create table app_sessions (
    tenant_id           uuid not null references tenants (id),
    id                  uuid not null,
    secret_hash         bytea not null check (length(secret_hash) = 32),
    user_id             uuid not null references users (id),
    platform_session_id uuid not null,
    app_node_id         uuid not null,
    environment         app_environment not null,
    deployment_id       uuid,
    created_at          timestamptz not null default now(),
    expires_at          timestamptz not null,
    ended_at            timestamptz,
    primary key (tenant_id, id),
    check ((environment = 'preview') = (deployment_id is not null))
);
create unique index app_sessions_secret_idx on app_sessions (tenant_id, secret_hash);
create index app_sessions_platform_idx on app_sessions (platform_session_id) where ended_at is null;

do $$
declare
    t text;
begin
    foreach t in array array['app_sign_in_codes', 'app_sessions'] loop
        execute format('alter table %I enable row level security', t);
        execute format('alter table %I force row level security', t);
        execute format(
            'create policy tenant_isolation on %I
                 using (tenant_id = ruum_current_tenant() or ruum_is_system())
                 with check (tenant_id = ruum_current_tenant() or ruum_is_system())', t);
    end loop;
end
$$;

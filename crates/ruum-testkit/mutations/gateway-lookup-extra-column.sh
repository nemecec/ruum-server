# The host lookup function returns another column (card G-1a). Security rule: docs/STEP3.md
# "System lookups": each narrow function returns only IDs, never general system access.
# Needs DATABASE_URL (eval "$(scripts/dev.sh env)").
DESCRIPTION="ruum_app_host_lookup returns the app slug too"
FILE="migrations/0013_app_host_lookup.sql"
PERL='s/returns table \(tenant_id uuid, app_id uuid, deployment_id uuid\)/returns table (tenant_id uuid, app_id uuid, deployment_id uuid, app_slug text)/; s/select s\.tenant_id, a\.node_id, d\.id\n/select s.tenant_id, a.node_id, d.id, a.slug\n/'
TESTS="-p ruum-gateway --test host_lookup_pg"
PLACEHOLDER=0

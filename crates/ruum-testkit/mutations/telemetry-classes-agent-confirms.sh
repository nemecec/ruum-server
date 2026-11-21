# An agent may confirm telemetry classes (card O-2). Security rule: confirmAppTelemetryFields is
# users only; an agent gets 403 user_required (design 9.6, docs/STEP3.md permission table).
DESCRIPTION="confirmAppTelemetryFields lets an agent confirm"
FILE="crates/ruum-core/src/telemetry/query.rs"
PERL='s/let by = crate::actor::require_user_actor\(ctx\)\?;\n(\s*)self\.app\(s, ctx, app, Role::Manage\)/let by = crate::UserId(ctx.actor.actor.id.0);\n$1self.app(s, ctx, app, Role::Manage)/'
TESTS="-p ruum-testkit --test apps_telemetry"
PLACEHOLDER=0

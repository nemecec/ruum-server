# An agent may compute a pseudonym of a known value (card O-2). Security rule: computeAppPseudonym
# is users only, so an agent cannot test guesses against the logs (decision 0006-8).
DESCRIPTION="computeAppPseudonym lets an agent compute"
FILE="crates/ruum-core/src/telemetry/query.rs"
PERL='s/let user = crate::actor::require_user_actor\(ctx\)\?;/let user = crate::UserId(ctx.actor.actor.id.0);/'
TESTS="-p ruum-testkit --test apps_telemetry"
PLACEHOLDER=0

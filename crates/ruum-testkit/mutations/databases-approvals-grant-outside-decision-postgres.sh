# The grant of an approved grant request is written in a transaction of its own (card A-2).
# Security rule: approving a grant request writes the grant in the same transaction as the
# decision (decision 0004-20, C-17), so a decision that fails leaves no grant. The conformance
# case makes the row refuse the decision after the grant was written. Needs DATABASE_URL
# (eval "$(scripts/dev.sh env)").
DESCRIPTION="Postgres decision commits the grant before the decision"
FILE="crates/ruum-core/src/databases/access/approvals/postgres.rs"
PERL='s/let \(gid, ob\) = write_grant\(&mut tx, ctx, user, revision, g, a\.id\)\.await\?;/let mut gtx = crate::sharing::postgres::begin(&self.pool, ctx).await?; let (gid, ob) = write_grant(&mut gtx, ctx, user, revision, g, a.id).await?; gtx.commit().await?;/'
TESTS="-p ruum-core --lib databases::access::approvals::tests::postgres_store_conformance -- --exact"
PLACEHOLDER=0

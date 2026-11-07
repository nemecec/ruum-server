# The mail worker sends a share mail also after the grant was removed.
# Rule: before it sends, the worker checks that a grant on the node still exists for the
# recipient; a removed grant drops the mail (decision 0003-3, reviewer 1 #3 (a)).
DESCRIPTION="the mail outbox sends a share mail for a removed grant"
FILE="crates/ruum-core/src/mailing/outbox.rs"
PERL='s/\("share", Some\(n\), _\) => share_alive\(&mut tx, tenant, n, &to\)\.await\?,/("share", Some(_), _) => true,/'
TESTS="-p ruum-core --lib sharing::conformance::postgres_mail_outbox_worker -- --exact"
PLACEHOLDER=0

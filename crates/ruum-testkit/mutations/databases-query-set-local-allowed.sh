# SET (also SET LOCAL) runs (card Q-1). Security rule: SET, SET LOCAL, RESET and set_config of
# any option never run, because the platform's options hold the limits of the call (decision
# 0004-35 a). The mutation drops SET from the first-word check and lets the parse accept it.
DESCRIPTION="the statement rules allow SET LOCAL"
FILE="crates/ruum-core/src/databases/query/rules.rs"
PERL='s/Some\("SET" \| "RESET" \| "DISCARD"\)/Some("RESET" | "DISCARD")/; s/Statement::Set\(_\) => return Err\(session_statement\(\)\),/Statement::Set(_) => Read,/'
TESTS="-p ruum-core --lib databases::query"
PLACEHOLDER=0

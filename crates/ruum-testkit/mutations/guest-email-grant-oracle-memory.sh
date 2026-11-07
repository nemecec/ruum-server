# The in-memory store: see guest-email-grant-oracle.sh (decision 0003-2, reviewer 1 #1).
DESCRIPTION="the guest email grant oracle while guests are off (memory)"
FILE="crates/ruum-core/src/sharing/memory.rs"
PERL='s/(Grantee::Email \{ email \} => \{\n\s*)if !policy\.allow_guests \{/${1}if false {/'
TESTS="-p ruum-core --lib sharing::conformance::memory_email_grants_conform -- --exact"
PLACEHOLDER=0

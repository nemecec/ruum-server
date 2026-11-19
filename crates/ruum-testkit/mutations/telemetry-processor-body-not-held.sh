# The HTTP receiver does not hold a body's declared length before it reads it (review of card
# O-1b). Security rule (S-61 fair share): the in-flight bytes are held from the moment a body
# is read, so slow bodies of one tenant fill its share and not more.
DESCRIPTION="the telemetry HTTP receiver reads bodies without holding their declared length"
FILE="crates/ruum-server/src/telemetry.rs"
PERL='s/r\.hold\(&identity, declared\.unwrap_or\(0\) as u64\)/r.hold(\&identity, 0)/'
TESTS="-p ruum-server telemetry"
PLACEHOLDER=0

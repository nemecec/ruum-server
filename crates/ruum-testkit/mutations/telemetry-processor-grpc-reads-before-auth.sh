# The gRPC receiver reads the message of a source it refuses (review of card O-1b). Security
# rule: an unknown source is refused from its peer address and headers; its body is never read
# or decoded.
DESCRIPTION="the telemetry gRPC receiver reads a stranger's message before it refuses it"
FILE="crates/ruum-server/src/telemetry.rs"
PERL='s/(        let identity = match r\.admit\(&facts\)\.await \{\n            Ok\(id\) => id,\n            )Err\(e\) => return e\.grpc_status\(\)\.into_http\(\),/$1Err(e) => { let _ = http_body_util::BodyExt::collect(req.into_body()).await; return e.grpc_status().into_http(); }/'
TESTS="-p ruum-server telemetry"
PLACEHOLDER=0

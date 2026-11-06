# WebDAV GET answers lose the content-safety headers.
# Security rule: user content never runs as a page (design section 15). The WebDAV host is
# apart from the platform origin (decision 0002-3); as defence in depth GET carries the content
# CSP (sandbox), nosniff and Content-Disposition: attachment, and active types go out as
# application/octet-stream (security review of 2026-11-06).
DESCRIPTION="WebDAV answers skip content_safety (no CSP, nosniff, attachment)"
FILE="crates/ruum-webdav/src/lib.rs"
PERL='s/\n\s*content_safety\(&method, &path, &mut res\);//'
TESTS="-p ruum-webdav --test dav files_are_served_as_sandboxed_attachments"
PLACEHOLDER=0

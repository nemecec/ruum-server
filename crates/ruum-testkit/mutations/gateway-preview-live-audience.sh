# The gateway gives a preview the live audience (card G-1a). Security rule: contracts/apps/README.md
# "Rules", docs/STEP3.md "How an app runs" rule 4: a preview's user token has
# aud app:<app>/preview/<deployment> and env preview, so the live app refuses it.
DESCRIPTION="the gateway uses the live aud on a preview"
FILE="crates/ruum-gateway/src/lib.rs"
PERL='s/token::preview_aud\(self\.resolved\.app_id, self\.resolved\.deployment_id\)/token::live_aud(self.resolved.app_id)/'
TESTS="-p ruum-gateway --test t6_world"
PLACEHOLDER=0

# A member with a lower content role manages the team tree again.
# Security rule: a member's content role limits what the member may do in the team tree; only
# members without a lower content role (content role `manage`) manage it (decision 0003-1).
DESCRIPTION="schema.zed drops the content roles from team_manager"
FILE="contracts/authz/schema.zed"
PERL='s/permission team_manager = member - \(content_viewer \+ content_commenter \+ content_editor\)/permission team_manager = member/'
TESTS="-p ruum-testkit --test permissions permission_matrix -- --exact"
PLACEHOLDER=0

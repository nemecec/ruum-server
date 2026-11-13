# A role that a new release removes keeps its `active` tuple (card B-2). Security rule: the
# grants of a removed role stay and give nothing, neither `use` nor `grant` (design 9.3,
# schema.zed `app_role#has = holder & active`). The test checks `use` and the roles of the holder
# after a release without the role.
DESCRIPTION="the live-release hook keeps 'active' of a removed role"
FILE="crates/ruum-core/src/apps/roles.rs"
PERL='s/ *updates\.push\(RelationshipUpdate::Delete\(active_tuple\(app, r\)\)\);\n//'
TESTS="-p ruum-core --lib apps::roles::tests::a_removed_role_keeps_its_grants_and_gives_nothing -- --exact"
PLACEHOLDER=0

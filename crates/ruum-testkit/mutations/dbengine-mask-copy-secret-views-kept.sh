# A masked copy makes again only the views over personal columns, not those over secret columns
# (backlog 2026-11-09 S-8). Rule: a materialized view of a masked copy holds no secret of the
# source. Since review FU-1 (required change 1) every materialized view of the own schema is
# made again too, which covers the same case; so the mutation turns that line off as well,
# else it changes nothing that can be seen.
DESCRIPTION="a materialized view over a secret column keeps the source's secrets in a masked copy"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/\.filter\(\|\(c, _\)\| c\.schema == "public"\)\n(\s+)\.map\(\|\(c, _\)\| \(format!/.filter(|(c, m)| c.schema == "public" \&\& *m == MaskAs::Pseudonym)\n$1.map(|(c, _)| (format!/; s/c\.relkind = \x27m\x27\),/c.relkind = \x27m\x27 and false),/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_copy_makes_views_over_masked_columns_again"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0

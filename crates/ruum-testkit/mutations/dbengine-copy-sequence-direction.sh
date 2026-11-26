# A masked or schema_only copy sets an owned sequence to the column's highest value whatever its
# direction, and beyond its bounds (review k16d3b of the D-3b fixes, Kimi N1). Correctness rule:
# the copy's next row of a descending sequence gets a new ID, and a value beyond MAXVALUE does
# not refuse the copy.
DESCRIPTION="a masked copy sets a descending sequence from the highest value, unbounded"
FILE="crates/ruum-dbengine/src/postgres/copy.rs"
PERL='s/greatest\(%L::int8, least\(%L::int8, m\)\)/m + 0 * %L::int8 + 0 * %L::int8/; s/case when s\.seqincrement < 0 then .min. else .max. end/\x27max\x27/'
TESTS="-p ruum-dbengine --lib -- postgres::tests::hostile_copy::a_masked_copy_sets_owned_sequences_in_their_direction_and_bounds"
ENV="RUUM_DBENGINE_REQUIRE_POSTGRES=1"
PLACEHOLDER=0

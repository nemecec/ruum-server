# The binding repair reads one page of a target's active rows and deletes every binding
# tuple that page does not imply, so a valid grant past the first page goes (Kimi run 13 B-1,
# finding 2). Security rule (availability of granted access): the repair deletes only
# tuples that no active row implies.
DESCRIPTION="the binding repair reads one page of a target's active rows"
FILE="crates/ruum-core/src/apps/bindings.rs"
PERL='s/let full = page\.len\(\) as u32 >= ROWS_PER_QUERY;/let full = page.len() as u32 >= u32::MAX;/'
TESTS="-p ruum-core --lib apps::bindings::tests::the_repair_reads_every_active_row_of_a_target -- --exact"
PLACEHOLDER=0

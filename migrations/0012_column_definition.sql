-- The definition of a computed column (card Q-2, review-Q-2-1 F-1).
--
-- `CREATE OR REPLACE VIEW`, `ALTER TABLE ... ALTER COLUMN ... SET EXPRESSION` and `CREATE OR
-- REPLACE FUNCTION` keep a column's `rel_oid`, `attnum` and `type_oid`, so the class of the
-- column would stay while the column computes something else (a column confirmed `normal` that
-- now reads a `secret` column). `definition_hash` is a hash of what a column of a view or a
-- generated column computes (the engine's `ColumnIdentity::definition`); a change clears the
-- confirmation with the reason `redefined`. A plain table column has no hash.

alter table database_columns add column definition_hash bigint;

alter table database_columns drop constraint database_columns_unconfirmed_reason_check;
alter table database_columns add constraint database_columns_unconfirmed_reason_check
    check (unconfirmed_reason is null or unconfirmed_reason in
           ('new', 'renamed', 'retyped', 'table_changed', 'recreated', 'redefined'));

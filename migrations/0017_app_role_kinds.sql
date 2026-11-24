-- The app role kinds of a binding (card D-5; decisions 0004-14 and 0004-32): `database_roles.kind`
-- takes `app_read` and `app_write`, one role per (database, kind, binding). The binding check
-- (`binding_id` set exactly for the app kinds) and the unique key exist since 0004.
--
-- Expand only (design 12.7, decision 42): the previous release never writes the new values, and
-- its reads of `database_roles` (the delete at purge, a count in a test) do not parse the kind.
alter table database_roles drop constraint database_roles_kind_check;
alter table database_roles add constraint database_roles_kind_check check (kind in (
    'read', 'write', 'owner', 'masked_read', 'masked_write', 'secret_read', 'mask',
    'app_read', 'app_write'));

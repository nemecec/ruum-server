-- The content role of team members (decision 0003-1).
--
-- `member_role` does not change. A new column `content_role` (`view | comment | edit | manage`)
-- says what a member may do in the team tree. It applies only to `role = 'member'`; the
-- default for members is `manage`, so the behaviour of existing members does not change.
-- Owners and admins always manage the team tree (`tenant#administer`) and have no content
-- role; guests have only grants and no content role.
--
-- One invariant, checked on both tables: `content_role` is not null if and only if
-- `role = 'member'`.
--
-- The SpiceDB graph is rebuilt from these rows (the mapping is in the header of
-- `contracts/authz/schema.zed`): a member with `view`, `comment` or `edit` also has the tuple
-- `tenant#content_viewer`, `tenant#content_commenter` or `tenant#content_editor`; a member with
-- `manage` has none. Existing members get `manage`, so no tuple has to be written.

-- `memberships` and `invitations` force row-level security; the migration works on all tenants.
select set_config('ruum.system', 'on', true);

alter table memberships add column content_role grant_role;
update memberships set content_role = 'manage' where role = 'member';
alter table memberships add constraint memberships_content_role_check
    check ((role = 'member') = (content_role is not null));

alter table invitations add column content_role grant_role;
update invitations set content_role = 'manage' where role = 'member';
alter table invitations add constraint invitations_content_role_check
    check ((role = 'member') = (content_role is not null));

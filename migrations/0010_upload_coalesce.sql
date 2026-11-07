-- The `coalesce` flag of an upload of a new version (C-8, decision 0003-6).
--
-- `UploadTargetNewVersion.coalesce` is stored with the upload session, because the version is
-- made at `completeUpload`. `false` makes a new version that never joins the head version
-- (the version policy's coalescing, design 5.3). Existing sessions keep the old behaviour
-- (`true`). The column is not named `coalesce`, which is an SQL keyword.

alter table uploads add column may_coalesce boolean not null default true;

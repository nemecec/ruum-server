-- The image of a node22 deployment (card I6-13; backlog I6-13, P-120).
--
-- Expand only (design 12.7, decision 42): one new nullable column. The previous release does
-- not read or write it (it refuses node22 deployments, so it makes no row that needs one), and
-- its inserts leave it null. A static deployment keeps it null.
--
-- `image_digest` is the digest of the OCI manifest that the builder pushed to the app's own
-- repository (`ruum/<tenant>/<app>` in the cluster registry). The stage `image` of a preview
-- sets it once; a release copies it from the deployment it publishes. The runtime driver runs
-- the image by this digest only, never by a tag.

alter table app_deployments
    add column image_digest text
        check (image_digest is null or image_digest ~ '^sha256:[0-9a-f]{64}$');

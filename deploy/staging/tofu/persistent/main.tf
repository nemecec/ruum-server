locals {
  tags = {
    env          = "staging"
    "managed-by" = "tofu"
    project      = "ruum"
  }

  # WAL archive and base backups of CNPG; the nightly copy of the Hetzner blob bucket
  # (rclone crypt, so the copy is unreadable at Scaleway, review 1 S-5).
  buckets = {
    pg_backup  = "ruum-staging-pg-backup"
    blobs_copy = "ruum-staging-blobs-copy"
  }
}

resource "scaleway_object_bucket" "backup" {
  for_each            = local.buckets
  name                = each.value
  object_lock_enabled = true
  tags                = merge(local.tags, { role = "backup" })

  versioning {
    enabled = true
  }

  # Barman retention is off (the in-cluster key has no delete right, review 1 B-3). WAL and base
  # backups older than the window expire by lifecycle, not by a key (PITR window: retention + 7
  # days; a base backup runs daily). The blob copy never expires current objects: rclone copies
  # a chunk once, so an expired copy would be gone for good. Scaleway keeps the non-current
  # versions that an expiry leaves; `destroy.sh --purge-backups` removes them (README "Costs").
  dynamic "lifecycle_rule" {
    for_each = each.key == "pg_backup" ? [1] : []
    content {
      id      = "expire-old-backups"
      enabled = true
      expiration {
        days = var.retention_days + 7
      }
    }
  }

  lifecycle_rule {
    id                                     = "abort-multipart"
    enabled                                = true
    abort_incomplete_multipart_upload_days = 2
  }

  lifecycle {
    prevent_destroy = true
  }
}

resource "scaleway_object_bucket_lock_configuration" "backup" {
  for_each = scaleway_object_bucket.backup
  bucket   = each.value.name

  rule {
    default_retention {
      mode = "COMPLIANCE"
      days = var.retention_days
    }
  }
}

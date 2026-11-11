# The destroyable part of staging: network, firewall, servers, load balancer, DNS records, the
# Hetzner blob bucket and the Talos bootstrap. `destroy.sh` removes exactly this root.
# The kept buckets are in ../persistent; the state bucket is made by bootstrap-state.sh.
terraform {
  required_version = "~> 1.13.0"

  required_providers {
    hcloud = {
      source  = "hetznercloud/hcloud"
      version = "1.70.2"
    }
    talos = {
      source  = "siderolabs/talos"
      version = "0.12.0"
    }
    minio = {
      source  = "aminueza/minio"
      version = "3.46.3"
    }
  }

  # State at Scaleway (kept on destroy), encrypted before it leaves the build server.
  # up.sh passes the bucket settings with -backend-config; the passphrase comes from
  # TF_VAR_state_passphrase (TOFU_STATE_PASSPHRASE in secrets.env), never from a file.
  backend "s3" {
    bucket                      = "ruum-staging-tfstate"
    key                         = "cluster.tfstate"
    region                      = "nl-ams"
    use_lockfile                = true
    skip_credentials_validation = true
    skip_region_validation      = true
    skip_requesting_account_id  = true
    skip_metadata_api_check     = true
    skip_s3_checksum            = true
    use_path_style              = true
    endpoints = {
      s3 = "https://s3.nl-ams.scw.cloud"
    }
  }

  encryption {
    key_provider "pbkdf2" "passphrase" {
      passphrase = var.state_passphrase
    }
    method "aes_gcm" "state" {
      keys = key_provider.pbkdf2.passphrase
    }
    state {
      method   = method.aes_gcm.state
      enforced = true
    }
    plan {
      method   = method.aes_gcm.state
      enforced = true
    }
  }
}

provider "hcloud" {
  # HCLOUD_TOKEN from the environment of the remote process (never a file, never the cluster).
}

provider "minio" {
  # Hetzner Object Storage. MINIO_USER / MINIO_PASSWORD come from the environment
  # (HETZNER_S3_ACCESS_KEY / HETZNER_S3_SECRET_KEY, mapped by up.sh).
  minio_server = "${var.location}.your-objectstorage.com"
  minio_region = var.location
  minio_ssl    = true
}

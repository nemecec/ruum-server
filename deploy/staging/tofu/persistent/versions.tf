# The kept part of staging: the two backup buckets at Scaleway (object lock, compliance mode).
# destroy.sh never destroys this root ("kept on purpose"). `destroy.sh --purge-backups` is a red
# action for the person after the retention has ended.
terraform {
  required_version = "~> 1.13.0"

  required_providers {
    scaleway = {
      source  = "scaleway/scaleway"
      version = "2.91.0"
    }
  }

  backend "s3" {
    bucket                      = "ruum-staging-tfstate"
    key                         = "persistent.tfstate"
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

provider "scaleway" {
  # SCW_ACCESS_KEY / SCW_SECRET_KEY / SCW_DEFAULT_PROJECT_ID from the environment: the full
  # key, on the person's Mac only, for OpenTofu. The cluster never gets it (review 1 B-3).
  region = "nl-ams"
  zone   = "nl-ams-1"
}

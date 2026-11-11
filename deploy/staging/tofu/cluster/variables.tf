variable "state_passphrase" {
  description = "Passphrase of the state encryption (TOFU_STATE_PASSPHRASE)."
  type        = string
  sensitive   = true
  validation {
    condition     = length(var.state_passphrase) >= 32
    error_message = "TOFU_STATE_PASSPHRASE must be at least 32 characters."
  }
}

variable "location" {
  description = "Hetzner location. Ruum starts in Helsinki (DESIGN 13)."
  type        = string
  default     = "fsn1"
}

variable "server_type" {
  description = "Server type of the nodes (versions.env HCLOUD_SERVER_TYPE)."
  type        = string
  default     = "cx23"
}

variable "node_count" {
  description = "Number of Talos nodes, all control plane and worker. 3, or 4 as the last step within the budget."
  type        = number
  default     = 3
  validation {
    condition     = var.node_count == 3 || var.node_count == 4
    error_message = "node_count is 3 (etcd quorum) or 4 (the last step within EUR 50, review 2 C-1)."
  }
}

variable "lb_type" {
  description = "Load balancer type."
  type        = string
  default     = "lb11"
}

variable "talos_image" {
  description = "ID of the Talos snapshot (label os=talos). up.sh finds it or uploads it first."
  type        = string
}

variable "talos_version" {
  description = "Talos version (versions.env TALOS_VERSION)."
  type        = string
}

variable "kubernetes_version" {
  description = "Kubernetes version (versions.env KUBERNETES_VERSION, without the v)."
  type        = string
}

variable "talos_installer_image" {
  description = "Installer image for upgrades (versions.env TALOS_INSTALLER_IMAGE)."
  type        = string
}

variable "admin_cidrs" {
  description = "The one source allowed to reach the Talos API (50000) and the Kubernetes API (6443): the current public IPv4 address of the operator's Mac as a /32. up.sh reads it at run time (steps.sh admin_cidrs)."
  type        = list(string)
  validation {
    condition     = length(var.admin_cidrs) == 1 && alltrue([for c in var.admin_cidrs : can(regex("^([0-9]{1,3}\\.){3}[0-9]{1,3}/32$", c))])
    error_message = "admin_cidrs must be exactly one IPv4 address as a /32 (the operator's Mac), never a wider range."
  }
}

variable "bucket_blobs" {
  description = "Hetzner bucket of the Ruum chunks (destroyed with staging)."
  type        = string
  default     = "ruum-staging-blobs-768usm"
}

variable "state_passphrase" {
  description = "Passphrase of the state encryption (TOFU_STATE_PASSPHRASE)."
  type        = string
  sensitive   = true
  validation {
    condition     = length(var.state_passphrase) >= 32
    error_message = "TOFU_STATE_PASSPHRASE must be at least 32 characters."
  }
}

variable "retention_days" {
  description = "Object lock retention of the backup buckets, compliance mode: no key can shorten it (review 1 B-3)."
  type        = number
  default     = 7
}

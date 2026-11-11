output "buckets" {
  value = { for k, b in scaleway_object_bucket.backup : k => b.name }
}

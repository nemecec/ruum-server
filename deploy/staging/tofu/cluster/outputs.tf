output "lb_ipv4" {
  value = hcloud_load_balancer.edge.ipv4
}

output "lb_ipv6" {
  value = hcloud_load_balancer.edge.ipv6
}

output "node_ipv4" {
  value = local.public_v4
}

output "node_names" {
  value = keys(local.nodes)
}

output "pg_generation" {
  description = "The backup path of this cluster's CNPG (serverName pg-<generation>)."
  value       = terraform_data.generation.output
}

output "talosconfig" {
  value     = data.talos_client_configuration.this.talos_config
  sensitive = true
}

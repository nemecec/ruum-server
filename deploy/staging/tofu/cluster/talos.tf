# Talos bootstrap, applied from the operator's Mac (where OpenTofu runs, in the tools container)
# over the Talos API, which the firewall opens only to admin_cidrs (the build server's public
# IPv4). The Talos resources reach the nodes through the admin SSH tunnel over the build server
# (backlog O-21, README "Admin access"). The machine config never goes through user_data, so no
# pod can read it from the metadata service (review 1 B-1).
#
# Destroy without node access (review 2 D-1): no talos_cluster_health and no
# talos_cluster_kubeconfig here (up.sh takes the kubeconfig with talosctl), and destroying the
# resources below does not contact the nodes. destroy.sh falls back to -refresh=false.

locals {
  cluster_name = "ruum-staging"
  # The API endpoint inside the cluster: the private address of stg-1. Nodes reach it over the
  # private network; the Mac uses the public addresses (in the cert SANs) through the tunnel.
  cluster_endpoint = "https://${local.nodes["stg-1"].private_ip}:6443"
  public_v4        = { for k, s in hcloud_server.node : k => s.ipv4_address }
}

# --- Talos API endpoints (backlog O-21) ---
# Each Talos resource dials its node through the admin tunnel: endpoint 127.0.0.1:1500N (the local
# port of tunnel.sh, TUNNEL_TALOS_BASE + N; tunnel-relay.sh serves it in the tools container), node
# by its public IPv4, so the endpoint's apid relays to itself. talos_endpoints holds the endpoint
# of the state for a resource that the plan does not create or change (steps.sh plan_cluster), so
# a plan without a Talos change shows no change; an endpoint is only used when the provider
# contacts the node (create, update).
locals {
  tunnel_endpoint    = { for k, n in local.nodes : k => "127.0.0.1:${15000 + n.index}" }
  talos_endpoint     = { for k, n in local.nodes : k => lookup(var.talos_endpoints, k, local.tunnel_endpoint[k]) }
  bootstrap_endpoint = lookup(var.talos_endpoints, "bootstrap", local.tunnel_endpoint["stg-1"])
}

resource "talos_machine_secrets" "this" {
  talos_version = var.talos_version
}

data "talos_machine_configuration" "node" {
  for_each           = local.nodes
  cluster_name       = local.cluster_name
  cluster_endpoint   = local.cluster_endpoint
  machine_type       = "controlplane"
  machine_secrets    = talos_machine_secrets.this.machine_secrets
  talos_version      = var.talos_version
  kubernetes_version = var.kubernetes_version
  docs               = false
  examples           = false
  # Talos v1.14 config: one document per setting (talos/patches/cluster.yaml explains why the
  # v1alpha1 document stays almost empty). tests/run.sh validates the same patches offline.
  config_patches = [
    file("${path.module}/../../talos/patches/cluster.yaml"),
    file("${path.module}/../../talos/patches/schedule-on-control-planes.yaml"),
    file("${path.module}/../../talos/patches/local-path-volume.yaml"),
    join("---\n", [
      yamlencode({
        machine = {
          certSANs = values(local.public_v4)
        }
      }),
      yamlencode({
        apiVersion = "v1alpha1"
        kind       = "UnattendedInstallConfig"
        installer = {
          image = var.talos_installer_image
        }
        provisioning = {
          diskSelector = {
            match = "disk.dev_path == \"/dev/sda\""
          }
        }
      }),
      yamlencode({
        apiVersion = "v1alpha1"
        kind       = "HostnameConfig"
        auto       = "off"
        hostname   = each.key
      }),
      yamlencode({
        apiVersion    = "v1alpha1"
        kind          = "KubeAPIServerConfig"
        certExtraSANs = concat(values(local.public_v4), [local.nodes["stg-1"].private_ip])
      }),
    ]),
  ]
}

data "talos_client_configuration" "this" {
  cluster_name         = local.cluster_name
  client_configuration = talos_machine_secrets.this.client_configuration
  endpoints            = values(local.public_v4)
  nodes                = values(local.public_v4)
}

resource "talos_machine_configuration_apply" "node" {
  for_each                    = local.nodes
  client_configuration        = talos_machine_secrets.this.client_configuration
  machine_configuration_input = data.talos_machine_configuration.node[each.key].machine_configuration
  node                        = hcloud_server.node[each.key].ipv4_address
  endpoint                    = local.talos_endpoint[each.key]
  apply_mode                  = "auto"

  lifecycle {
    # Only this node's tunnel port, or its own public address kept from the state.
    precondition {
      condition     = contains([local.tunnel_endpoint[each.key], hcloud_server.node[each.key].ipv4_address], local.talos_endpoint[each.key])
      error_message = "The Talos endpoint of a node is its admin tunnel port 127.0.0.1:1500N or its own public IPv4 (backlog O-21)."
    }
  }
}

resource "talos_machine_bootstrap" "this" {
  client_configuration = talos_machine_secrets.this.client_configuration
  node                 = hcloud_server.node["stg-1"].ipv4_address
  endpoint             = local.bootstrap_endpoint
  depends_on           = [talos_machine_configuration_apply.node]

  lifecycle {
    precondition {
      condition     = contains([local.tunnel_endpoint["stg-1"], hcloud_server.node["stg-1"].ipv4_address], local.bootstrap_endpoint)
      error_message = "The bootstrap endpoint is stg-1's admin tunnel port 127.0.0.1:15001 or its own public IPv4 (backlog O-21)."
    }
  }
}

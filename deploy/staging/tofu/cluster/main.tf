locals {
  # Every resource that can carry labels carries these (check-plan.sh refuses any that does not).
  labels = {
    env          = "staging"
    "managed-by" = "tofu"
    project      = "ruum"
  }
  node_labels = merge(local.labels, { role = "node" })

  network_cidr = "10.20.0.0/16"
  subnet_cidr  = "10.20.1.0/24"
  lb_ip        = "10.20.1.5"
  nodes = {
    for i in range(1, var.node_count + 1) : "stg-${i}" => {
      index      = i
      private_ip = cidrhost(local.subnet_cidr, 10 + i)
    }
  }
}

# --- DNS zones: data sources only (review 1 B-9). Never an hcloud_zone resource. ---
data "hcloud_zone" "ruum_space" {
  name = "ruum.space"
}

data "hcloud_zone" "ruumapps_net" {
  name = "ruumapps.net"
}

# The build server's key, only so Hetzner sends no root password mail. Talos ignores it.
data "hcloud_ssh_key" "buildbox" {
  name = "ruum-buildbox"
}

# --- Network ---
resource "hcloud_network" "staging" {
  name     = "ruum-staging"
  ip_range = local.network_cidr
  labels   = local.labels
}

resource "hcloud_network_subnet" "nodes" {
  network_id   = hcloud_network.staging.id
  type         = "cloud"
  network_zone = "eu-central"
  ip_range     = local.subnet_cidr
}

# --- Firewall: attached at server create (review 1 B-2), IPv4 and IPv6. ---
# Hetzner firewalls do not filter the private network: the load balancer and node-to-node
# traffic (etcd, kubelet, Flannel) pass over it. Nothing else is open on the public addresses.
resource "hcloud_firewall" "nodes" {
  name   = "ruum-staging-nodes"
  labels = local.labels

  rule {
    description = "Talos API from the build server"
    direction   = "in"
    protocol    = "tcp"
    port        = "50000"
    source_ips  = var.admin_cidrs
  }

  rule {
    description = "Kubernetes API from the build server"
    direction   = "in"
    protocol    = "tcp"
    port        = "6443"
    source_ips  = var.admin_cidrs
  }

  rule {
    description = "ICMP from the build server (review stgsec S-13)"
    direction   = "in"
    protocol    = "icmp"
    source_ips  = var.admin_cidrs
  }
}

resource "hcloud_placement_group" "nodes" {
  name   = "ruum-staging-nodes"
  type   = "spread"
  labels = local.labels
}

# --- Public addresses: explicit, labelled, so a sweep by label finds them (review 2 D-2). ---
resource "hcloud_primary_ip" "v4" {
  for_each    = local.nodes
  name        = "${each.key}-v4"
  type        = "ipv4"
  location    = var.location
  auto_delete = false
  labels      = local.node_labels
}

resource "hcloud_primary_ip" "v6" {
  for_each    = local.nodes
  name        = "${each.key}-v6"
  type        = "ipv6"
  location    = var.location
  auto_delete = false
  labels      = local.node_labels
}

# --- Servers: Talos snapshot, no user_data (review 1 B-1), firewall from the first boot. ---
# The node boots into Talos maintenance mode. talos_machine_configuration_apply below sends the
# machine config from the build server over the firewalled Talos API.
resource "hcloud_server" "node" {
  for_each           = local.nodes
  name               = each.key
  server_type        = var.server_type
  image              = var.talos_image
  location           = var.location
  labels             = local.node_labels
  firewall_ids       = [hcloud_firewall.nodes.id]
  placement_group_id = hcloud_placement_group.nodes.id
  ssh_keys           = [data.hcloud_ssh_key.buildbox.id]
  backups            = false

  public_net {
    ipv4_enabled = true
    ipv4         = hcloud_primary_ip.v4[each.key].id
    ipv6_enabled = true
    ipv6         = hcloud_primary_ip.v6[each.key].id
  }

  network {
    network_id = hcloud_network.staging.id
    ip         = each.value.private_ip
  }

  lifecycle {
    # A new snapshot must not replace running nodes: upgrades go through talosctl
    # (ops/upgrade-talos.sh), one node at a time.
    ignore_changes = [image, ssh_keys]
  }

  depends_on = [hcloud_network_subnet.nodes]
}

# --- Load balancer (made by OpenTofu, never by a cloud controller in the cluster) ---
resource "hcloud_load_balancer" "edge" {
  name               = "ruum-staging"
  load_balancer_type = var.lb_type
  location           = var.location
  labels             = local.labels
}

resource "hcloud_load_balancer_network" "edge" {
  load_balancer_id = hcloud_load_balancer.edge.id
  network_id       = hcloud_network.staging.id
  ip               = local.lb_ip
  depends_on       = [hcloud_network_subnet.nodes]
}

resource "hcloud_load_balancer_target" "nodes" {
  load_balancer_id = hcloud_load_balancer.edge.id
  type             = "label_selector"
  label_selector   = "env=staging,role=node"
  use_private_ip   = true
  depends_on       = [hcloud_load_balancer_network.edge]
}

resource "hcloud_load_balancer_service" "http" {
  load_balancer_id = hcloud_load_balancer.edge.id
  protocol         = "tcp"
  listen_port      = 80
  destination_port = 30080
  proxyprotocol    = true

  health_check {
    protocol = "tcp"
    port     = 30443
    interval = 10
    timeout  = 5
    retries  = 3
  }
}

resource "hcloud_load_balancer_service" "https" {
  load_balancer_id = hcloud_load_balancer.edge.id
  protocol         = "tcp"
  listen_port      = 443
  destination_port = 30443
  proxyprotocol    = true

  health_check {
    protocol = "tcp"
    port     = 30443
    interval = 10
    timeout  = 5
    retries  = 3
  }
}

# --- Hetzner Object Storage: the chunks of Ruum (destroyed with staging) ---
resource "minio_s3_bucket" "blobs" {
  bucket         = var.bucket_blobs
  acl            = "private"
  force_destroy  = true
  object_locking = false
}

# --- The generation of this cluster: a new CNPG backup path per create (review 2 R-2). ---
resource "terraform_data" "generation" {
  input = formatdate("YYYYMMDDhhmm", timestamp())
  lifecycle {
    ignore_changes = [input]
  }
}

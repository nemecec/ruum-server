# DNS records of staging (plan section 7). Only these names; check-plan.sh refuses any other
# rrset and any hcloud_zone resource. The notify.staging records (Scaleway email) are not here
# and are never touched.

locals {
  lb_v4 = hcloud_load_balancer.edge.ipv4
  lb_v6 = hcloud_load_balancer.edge.ipv6

  # CAA: Let's Encrypt (staging CA during bring-up, check-plan allows it) and Sectigo (ZeroSSL,
  # the trusted CA of staging that does not share production's Let's Encrypt quota, review 2 R-3).
  # No wildcard certificates (on-demand TLS issues per host).
  caa = [
    "0 issue \"letsencrypt.org\"",
    "0 issue \"sectigo.com\"",
    "0 issuewild \";\"",
  ]

  space_hosts = ["staging", "auth.staging", "dav.staging"]
  apps_hosts  = ["content.staging", "*.content.staging"]
}

resource "hcloud_zone_rrset" "space_a" {
  for_each = toset(local.space_hosts)
  zone     = data.hcloud_zone.ruum_space.name
  name     = each.value
  type     = "A"
  ttl      = 300
  labels   = local.labels
  records  = [{ value = local.lb_v4 }]
}

resource "hcloud_zone_rrset" "space_aaaa" {
  for_each = toset(local.space_hosts)
  zone     = data.hcloud_zone.ruum_space.name
  name     = each.value
  type     = "AAAA"
  ttl      = 300
  labels   = local.labels
  records  = [{ value = local.lb_v6 }]
}

resource "hcloud_zone_rrset" "apps_a" {
  for_each = toset(local.apps_hosts)
  zone     = data.hcloud_zone.ruumapps_net.name
  name     = each.value
  type     = "A"
  ttl      = 300
  labels   = local.labels
  records  = [{ value = local.lb_v4 }]
}

resource "hcloud_zone_rrset" "apps_aaaa" {
  for_each = toset(local.apps_hosts)
  zone     = data.hcloud_zone.ruumapps_net.name
  name     = each.value
  type     = "AAAA"
  ttl      = 300
  labels   = local.labels
  records  = [{ value = local.lb_v6 }]
}

resource "hcloud_zone_rrset" "space_caa" {
  zone    = data.hcloud_zone.ruum_space.name
  name    = "staging"
  type    = "CAA"
  ttl     = 300
  labels  = local.labels
  records = [for v in local.caa : { value = v }]
}

resource "hcloud_zone_rrset" "apps_caa" {
  zone    = data.hcloud_zone.ruumapps_net.name
  name    = "content.staging"
  type    = "CAA"
  ttl     = 300
  labels  = local.labels
  records = [for v in local.caa : { value = v }]
}

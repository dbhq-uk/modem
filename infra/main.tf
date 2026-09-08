# The infrastructure behind modem.dbhq.uk.
#
# WHAT IS AND IS NOT A SECRET, because the distinction decides what may live
# in a public repository and getting it wrong in either direction is
# expensive.
#
#   NOT SECRET, and deliberately committed:
#     account id, zone id, hostnames, the pages.dev target. These are
#     identifiers. They appear in every API call the account makes, and
#     treating an identifier as a secret buys nothing while costing the reader
#     the ability to understand what is deployed.
#
#   SECRET, and never in this repository in any form:
#     the API token. Supplied as TF_VAR_cloudflare_api_token at apply time,
#     from 1Password.
#
#   THE ACTUAL RISK, which is neither of those:
#     terraform state. It must never be in git, public OR private. It lives in
#     this project's own R2 bucket - see backend.hcl for why its own.
#
# A note on the Pages project itself: it is direct-upload, so Cloudflare never
# builds from this repo and serves exactly what the deploy workflow last
# uploaded. The zone runs HSTS with includeSubDomains, so this hostname has to
# serve valid HTTPS from its very first request - a browser that has already
# seen dbhq.uk will refuse to load it otherwise, which rules out ever parking
# this subdomain on anything without a certificate.

terraform {
  required_version = ">= 1.6"

  required_providers {
    cloudflare = {
      source  = "cloudflare/cloudflare"
      version = "~> 4.20"
    }
  }

  # Configured at init from backend.hcl:
  #   terraform init -backend-config=backend.hcl
  backend "s3" {}
}

provider "cloudflare" {
  # From TF_VAR_cloudflare_api_token. Never a default, never written here.
  api_token = var.cloudflare_api_token
}

resource "cloudflare_pages_project" "modem" {
  account_id        = var.account_id
  name              = var.pages_project
  production_branch = "main"

  lifecycle {
    # Deploy config is managed out of band, by this repo's own deploy
    # workflow driving wrangler. Terraform owns the project's existence and
    # its custom domain, not its deployments.
    ignore_changes = [build_config, deployment_configs, source]
  }
}

resource "cloudflare_pages_domain" "modem" {
  account_id   = var.account_id
  project_name = cloudflare_pages_project.modem.name
  domain       = var.hostname
}

# Proxied, unlike heliograph's record. heliograph is on GitHub Pages, which
# cannot complete its certificate challenge through Cloudflare's proxy;
# Cloudflare Pages has no such problem, and proxying is what puts the zone's
# own headers and caching in front of it.
resource "cloudflare_record" "modem" {
  zone_id = var.zone_id
  name    = "modem"
  type    = "CNAME"
  content = var.pages_target
  proxied = true
  ttl     = 1
  comment = "modem.dbhq.uk (Cloudflare Pages, project: modem)"
}

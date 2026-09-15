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

# KV for the acoustic lab (/lab), which drives two real devices in a room
# and collects what they measure.
#
# The devices poll for an instruction and post their measurements; both
# sides of that live here. `web/_worker.js` is the only thing that writes
# to it, and the operator reads it back over the REST API - see
# scripts/lab.py.
#
# Nothing in here is durable or valuable: every key the worker writes
# carries a seven-day TTL, and the plan key is rewritten on every run.
# Losing this namespace costs one calibration session, not data.
resource "cloudflare_workers_kv_namespace" "lab" {
  account_id = var.account_id
  title      = "modem-lab"
}

resource "cloudflare_pages_project" "modem" {
  account_id        = var.account_id
  name              = var.pages_project
  production_branch = "main"

  # Both environments get the same binding. Preview is not used by this
  # project's deploy workflow, which always publishes to `main`, but a
  # binding that exists in only one of them is the kind of asymmetry that
  # wastes an afternoon the first time someone does use it.
  deployment_configs {
    production {
      compatibility_date = "2026-09-08"
      # Pinned to what the project already runs. The provider's own
      # default is the legacy "bundled" model, so leaving this out makes
      # every plan propose a silent downgrade of the account's usage
      # model - a billing change, offered as drift.
      usage_model = "standard"
      kv_namespaces = {
        LAB = cloudflare_workers_kv_namespace.lab.id
      }
    }
    preview {
      compatibility_date = "2026-09-08"
      usage_model        = "standard"
      kv_namespaces = {
        LAB = cloudflare_workers_kv_namespace.lab.id
      }
    }
  }

  lifecycle {
    # Deployments are managed out of band, by this repo's own deploy
    # workflow driving wrangler. Terraform owns the project's existence,
    # its custom domain and its bindings, not what has been uploaded to
    # it.
    #
    # `deployment_configs` used to be on this list too, which was right
    # while there was nothing in it - and wrong the moment the lab needed
    # a KV binding, because a binding is not a deployment. Checked
    # against the live project before narrowing it: the only things in
    # deployment_configs were the compatibility date and defaults
    # Cloudflare sets itself, so there was nothing here for Terraform to
    # clobber.
    ignore_changes = [build_config, source]
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

# ---------------------------------------------------------------------------
# The code must revalidate, and only the zone can say so.
#
# Cloudflare Pages serves .js and .css with its own
# `public, max-age=14400, must-revalidate` - four hours in the *browser*
# cache - and `web/_headers` cannot override it. That was measured on the
# live site in three shapes: `/*.js` and `/*.css` blocks (Pages' _headers
# wildcard is a path splat, not a glob, so a suffix pattern matches
# nothing), a `/*` block, and every file named outright. Only the HTML
# routes ever changed. Pages sets the header on static assets after
# _headers is applied and wins.
#
# Nothing in web/ carries a content hash in its filename, so four hours
# means a returning visitor runs today's HTML against yesterday's
# JavaScript, and the deploy's edge purge cannot help - it clears
# Cloudflare's copy, not the one on the visitor's disk. On 9 Sep 2026 the
# fix for a `modem.wasm` 404 was live, correct at the origin and still
# invisible in the browser for exactly this reason, which cost most of an
# afternoon: the deployed bytes and the running bytes were different
# things and every check of the former said the site was fine.
#
# A response-header transform runs at the zone, after Pages, so it is the
# one place that can actually set this. `no-cache` means "store it, but
# revalidate before reuse" - the ordinary response becomes a 304 with no
# body, and the ETags already exist.
#
# The better answer is hashed filenames plus `immutable`, which wants a
# build step this site's plain JS does not have. If one ever arrives,
# delete this rule rather than leaving both.
#
# Applied 10 Sep 2026 and verified: .js, .css, the HTML and the WASM all
# return `no-cache` at the edge.
#
# Applying it was blocked for a day by something worth recording, because
# the symptom pointed the wrong way. `terraform init` returned 403 on
# HeadObject against dbhq-modem-tfstate, which reads as "the credential
# cannot see this bucket" - and the first conclusion drawn from it, that
# the R2 token needed widening and that heliograph was locked out too,
# was wrong on both counts. The token `dbhq - R2 terraform state` already
# granted Bucket Item Read+Write on four buckets. One entry named
# `modem-tfstate`; the bucket is `dbhq-modem-tfstate`. It held access to a
# bucket that does not exist and none to the one that does.
#
# Fixed by correcting that one resource name on the existing token - no
# new credential, and no key roll, since editing a policy does not change
# the access key or secret the loader holds.
#
# Underneath it is a naming inconsistency worth knowing about: three state
# buckets are `dbhq-` prefixed and heliograph's is not, so the token entry
# looks like it was written to heliograph's convention while the bucket
# was created to dbhq's. Standardising is a separate job and not one to
# start while live state sits in them.
resource "cloudflare_ruleset" "modem_code_revalidates" {
  zone_id     = var.zone_id
  name        = "modem.dbhq.uk - code revalidates"
  description = "Pages caches .js/.css for 4h and _headers cannot override it; the code carries no content hash, so it must revalidate."
  kind        = "zone"
  phase       = "http_response_headers_transform"

  rules {
    action      = "rewrite"
    description = "no-cache on modem.dbhq.uk scripts and styles"
    enabled     = true
    expression  = "(http.host eq \"${var.hostname}\" and (http.request.uri.path.extension eq \"js\" or http.request.uri.path.extension eq \"css\"))"

    action_parameters {
      headers {
        name      = "Cache-Control"
        operation = "set"
        value     = "no-cache"
      }
    }
  }
}

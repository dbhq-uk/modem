variable "cloudflare_api_token" {
  description = "Cloudflare API token. From TF_VAR_cloudflare_api_token, sourced from 1Password by ~/.dbhq/env.sh. Never has a default."
  type        = string
  sensitive   = true
}

variable "account_id" {
  description = "Cloudflare account id. An identifier, not a credential."
  type        = string
  default     = "691c21cdcf1b3fa4add70cc166e99733"
}

variable "zone_id" {
  description = "Zone id for dbhq.uk. The zone itself is managed by the DBHQ repo; this project only adds a record to it."
  type        = string
  default     = "48bb46832a2853526a1082accdac4147"
}

variable "hostname" {
  description = "Where the page lives."
  type        = string
  default     = "modem.dbhq.uk"
}

variable "pages_project" {
  description = "Cloudflare Pages project name."
  type        = string
  default     = "modem"
}

variable "pages_target" {
  description = "The pages.dev hostname the custom domain CNAMEs to. Cloudflare appends a suffix when the bare project name is already taken globally, which it was - so this is modem-9e4, not modem. Read it from the project rather than assuming it: guessing produces a CNAME pointing at a host that does not exist, and the site simply never resolves."
  type        = string
  default     = "modem-9e4.pages.dev"
}

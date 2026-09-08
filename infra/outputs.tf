output "site_url" {
  description = "Where the page is served."
  value       = "https://${var.hostname}"
}

output "pages_target" {
  description = "The pages.dev host the custom domain resolves to."
  value       = var.pages_target
}

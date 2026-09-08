# Backend configuration for this project's own R2 state bucket.
#
# Committed on purpose. Every value here is an identifier, not a credential:
# a bucket name and an account-scoped endpoint. The keys that open it come
# from 1Password at init time as AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY,
# by way of `source ~/.dbhq/env.sh`, and are never written to disk.
#
# The bucket is modem's alone. These three resources first went into the DBHQ
# repo's shared state, and while they sat there a plan from a different
# project in that same state proposed destroying all three - they were present
# in the state and absent from the configuration on that branch. Separate
# state makes that impossible rather than merely unlikely: no other
# configuration can see these resources, so none of them can plan to remove
# them. heliograph and bbs are both laid out this way.
#
#   terraform init -backend-config=backend.hcl

bucket = "dbhq-modem-tfstate"
key    = "modem.tfstate"
region = "auto"

endpoints = { s3 = "https://691c21cdcf1b3fa4add70cc166e99733.r2.cloudflarestorage.com" }

# R2 is S3-compatible, not S3. Each of these switches off a check that assumes
# a real AWS endpoint on the other end, and each one fails the init without it.
skip_credentials_validation = true
skip_region_validation      = true
skip_requesting_account_id  = true
skip_s3_checksum            = true
use_path_style              = true

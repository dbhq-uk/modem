# infra

The infrastructure behind `modem.dbhq.uk`: a Cloudflare Pages project, its
custom domain, and one DNS record in the `dbhq.uk` zone.

```bash
source ~/.dbhq/env.sh                     # Cloudflare + R2 keys, from 1Password
export TF_VAR_cloudflare_api_token="$CLOUDFLARE_API_TOKEN"
terraform init -backend-config=backend.hcl
terraform plan
```

## State is this project's alone, and that is the point

The state lives in the R2 bucket `dbhq-modem-tfstate`, not on disk and not in
git.

These three resources first went into the DBHQ repo's shared state, alongside
the website. While they sat there, a plan run from a **different** project in
that same state came back proposing to destroy all three: they were present in
the state and absent from the configuration on that branch. Nothing was lost,
but only because nobody typed yes.

Separate state per project makes that impossible rather than merely unlikely.
This configuration cannot see any other project's resources, and no other
configuration can see these, so none of them can plan to remove them.
`heliograph` and `bbs` are both laid out the same way.

## What is not managed here

The **zone** itself, and everything else in it, belongs to the DBHQ repo. This
project only adds a record to it, which is why `zone_id` is a plain variable
rather than a resource reference.

**Deployments** are not managed here either. The Pages project is
direct-upload: `.github/workflows/deploy.yml` drives wrangler, and Cloudflare
serves exactly what that workflow last uploaded. Terraform owns the project's
existence and its custom domain, not its contents - hence the `ignore_changes`
on the deploy config, which would otherwise churn on every push.

## The pages.dev target has a suffix

`modem.pages.dev` was already taken globally, so Cloudflare assigned
`modem-9e4.pages.dev`. Read it from the project rather than assuming it: a
CNAME pointing at a host that does not exist simply never resolves, and the
failure looks like a DNS problem rather than a naming one. `bbs` has the same
shape.

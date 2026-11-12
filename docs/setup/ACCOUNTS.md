# Accounts to set up (one sitting)

This page lists the accounts that only a person can create for Ruum. It takes about one hour in
total. Do the steps in order. The agents do everything else (servers, buckets, DNS records,
configuration) with the keys you create here.

Web consoles change their menus often. If a label here does not match what you see, look for the
nearest equivalent. The goal of each step is in its first line.

## Where to put the keys

Never paste a key in chat, an issue or the repository (the repository is public). Put all keys in
one file on your Mac:

```bash
mkdir -p ~/.config/ruum && touch ~/.config/ruum/secrets.env && chmod 600 ~/.config/ruum/secrets.env
```

Open the file in a text editor and add one line per key, `NAME=value`, with the names given
below. When you are done, tell the agents "secrets are in place". They read only that file and
only report which names are present, never the values.

## The names (overview)

| Name | From | Step |
|---|---|---|
| `HCLOUD_TOKEN` | Hetzner Cloud API token | 1 |
| `HETZNER_S3_ACCESS_KEY`, `HETZNER_S3_SECRET_KEY` | Hetzner Object Storage credentials | 1 |
| `HETZNER_DNS_TOKEN` | only if Hetzner DNS asks for its own token | 2 |
| `SCW_ACCESS_KEY`, `SCW_SECRET_KEY`, `SCW_DEFAULT_PROJECT_ID`, `SCW_DEFAULT_ORGANIZATION_ID` | Scaleway API key, project and organisation (backups and email) | 3 |
| `SCW_SMTP_ACCESS_KEY`, `SCW_SMTP_SECRET_KEY` | Scaleway SMTP key: Zitadel's outgoing email | 3 |
| `GOOGLE_OAUTH_CLIENT_ID`, `GOOGLE_OAUTH_CLIENT_SECRET` | Google sign-in | 4 |
| `MS_OAUTH_CLIENT_ID`, `MS_OAUTH_CLIENT_SECRET` | Microsoft sign-in | 5 |
| `STRIPE_SECRET_KEY`, `STRIPE_PUBLISHABLE_KEY` | Stripe, test mode | 6 |

## The names of the Ruum addresses

The steps below need these addresses. They are decided here (provisional; the agents may change
them only with a decision record):

| Address | What |
|---|---|
| `ruum.space`, `staging.ruum.space` | the web UI and the API |
| `auth.ruum.space`, `auth.staging.ruum.space` | sign-in (Zitadel) |
| `*.ruumapps.net`, `*.staging.ruumapps.net` | user apps and user content |
| `notify.ruum.space`, `notify.staging.ruum.space` | the sending domains for email |

## 1. Hetzner (servers and object storage), about 15 minutes

Goal: a Hetzner Cloud project for staging, an API token and object storage keys.

1. Go to <https://console.hetzner.com> and create an account. Hetzner may ask for an ID check or a
   payment method; that can take a few hours.
2. Create a project named **ruum-staging**. (Production gets its own project later.)
3. In the project: **Security → API tokens → Generate API token**. Name `ruum-agents`, permission
   **Read & Write**. Copy the token into `HCLOUD_TOKEN`. It is shown only once.
4. In the project: **Object Storage → S3 credentials → Generate credentials** (or **Security →
   S3 credentials**). Copy the access key and the secret key into `HETZNER_S3_ACCESS_KEY` and
   `HETZNER_S3_SECRET_KEY`. Do not create buckets; the agents do that, in Helsinki (`hel1`).
5. You do not need to add an SSH key. The agents make one and register it with the token.

Cost: staging is about 3 small servers, a load balancer and object storage, roughly 30-50 euros
per month. The agents create nothing that costs money until you say "start staging".

## 2. DNS at Hetzner, about 5 minutes

Goal: Hetzner answers DNS for `ruum.space` and `ruumapps.net`, so the agents can manage the records.

1. In the Hetzner Console, open **DNS** and add the zones `ruum.space` and `ruumapps.net`. Hetzner
   shows its name servers (three names, for example `hydrogen.ns.hetzner.com`).
2. At the registrar of each domain, replace the name servers with Hetzner's. The change can take up
   to a day to spread.
3. If the DNS page asks you for its own API token (the older `dns.hetzner.com` console), create one
   and save it as `HETZNER_DNS_TOKEN`. If DNS is inside the Cloud project, the `HCLOUD_TOKEN` is
   enough.
4. If `ruumapps.com` is registered too, add it the same way. It will only redirect.

## 3. Scaleway (off-site backups and email), about 15 minutes

Goal: one Scaleway account gives a second provider for backups (in Amsterdam, another country and
another company than Hetzner) and the service that sends email (invitations, sign-in mails). It is
French, so the data stays in the EU. One account for two needs means fewer keys.

1. Go to <https://console.scaleway.com> and create an account (a card is needed; there may be an ID
   check).
2. Create a project named **ruum-staging**. Copy its project ID into `SCW_DEFAULT_PROJECT_ID`, and the
   organisation ID (in the organisation settings) into `SCW_DEFAULT_ORGANIZATION_ID`; the Scaleway CLI and
   Terraform read both names.
   Production gets its own project (`ruum-prod`) later, as at Hetzner, so that a staging key can never
   read or delete production backups or send email as production.
3. **IAM → Applications → Create application** named `ruum-agents`. Give it a policy for the
   project **ruum-staging** with the permission sets **ObjectStorageFullAccess** and
   **TransactionalEmailFullAccess**. (A narrow key: it cannot touch anything else.)
4. **IAM → API keys → Generate API key** for the application `ruum-agents`. When Scaleway asks
   "Will this API key be used for Object Storage?", answer **Yes** and choose **ruum-staging** as the
   preferred project (S3 requests with this key then go to that project's buckets). Copy the access
   key and the secret key into `SCW_ACCESS_KEY` and `SCW_SECRET_KEY`.
5. **Transactional Email**: Scaleway first asks for a plan. Choose the entry plan (pay as you go, called
   "Essential" when this was written), with no dedicated IP. That is enough for staging and for the first
   users; a dedicated IP only helps at high volume. Then **Add domain** `notify.staging.ruum.space` (production adds `notify.ruum.space`
   in its own project later). Scaleway shows DNS records (SPF, DKIM,
   DMARC, MX). You do not need to add them by hand: once step 2 is active, the agents add them
   through the Hetzner DNS API and start the check.
6. A new Transactional Email project starts with a low sending limit. That is enough for staging.
   Before real users, ask Scaleway support to raise it (the agents will remind you).
7. **SMTP key** (Zitadel's outgoing email, backlog O-20). A separate application, so that the key
   in the cluster can only send email. Do the steps in this order:
   1. **IAM → Applications → Create application** named `ruum-staging-smtp`. Do not attach a
      policy in this dialog.
   2. **IAM → Policies → Create policy**. Principal: the application `ruum-staging-smtp`. Then add
      a rule: choose the **permission set first**, **TransactionalEmailEmailSmtpCreate** (not
      TransactionalEmailEmailApiCreate, which is for the HTTP API only and does not allow SMTP),
      then the **scope**: Project **ruum-staging**.
   3. **IAM → API keys → Generate API key** for the application `ruum-staging-smtp`. When Scaleway
      asks "Will this API key be used for Object Storage?", answer **No**.
   4. Copy the access key and the secret key into `SCW_SMTP_ACCESS_KEY` and `SCW_SMTP_SECRET_KEY`.
      The SMTP login is the project ID (`SCW_DEFAULT_PROJECT_ID`) as the user and
      `SCW_SMTP_SECRET_KEY` as the password, on `smtp.tem.scaleway.com` port 587 (STARTTLS).
      Never use `SCW_SECRET_KEY` for this: that key can also reach Object Storage.

Nothing to do for backups beyond the key; the agents create the bucket in `nl-ams`, with object
lock (write once) where available.

Status of the sending domain (read through the Scaleway API on 2026-11-07, no email sent):
`notify.staging.ruum.space` exists in region `fr-par` of the project, with status `checked`
(Scaleway's verified state), reputation `excellent`, last valid check 2026-11-07 04:55 UTC, next
check 2026-11-08 05:43 UTC, and 0 emails sent. Public DNS has its SPF, DKIM, DMARC (`p=none`)
and MX records. The field `last_error` still holds "record not found" for all four records, from
a check before the records existed.

## 4. Google sign-in, about 10 minutes

Goal: "Sign in with Google" for Ruum, through Zitadel.

1. Go to <https://console.cloud.google.com> and create a project named **ruum**.
2. Open **Google Auth Platform** (formerly "OAuth consent screen"):
   - **Branding**: app name **Ruum**, support email, the home page `https://ruum.space`, the
     authorised domain `ruum.space`. Skip the logo for now; a logo starts a verification review.
   - **Audience**: user type **External**. When the rest is done, press **Publish app** so that
     any Google account can sign in. With only the basic scopes below, Google does not need a
     review.
   - **Data access**: the scopes `openid`, `.../auth/userinfo.email` and `.../auth/userinfo.profile`
     only.
3. **Clients → Create client**, type **Web application**, name `ruum-zitadel`. Add these
   **Authorised redirect URIs** (all of them; they cover the laptop, staging and production, and
   both Zitadel login versions):
   ```text
   http://localhost:8180/ui/login/login/externalidp/callback
   http://localhost:8180/idps/callback
   https://auth.staging.ruum.space/ui/login/login/externalidp/callback
   https://auth.staging.ruum.space/idps/callback
   https://auth.ruum.space/ui/login/login/externalidp/callback
   https://auth.ruum.space/idps/callback
   ```
4. Copy the client ID and the client secret into `GOOGLE_OAUTH_CLIENT_ID` and
   `GOOGLE_OAUTH_CLIENT_SECRET`.

## 5. Microsoft sign-in, about 10 minutes

Goal: "Sign in with Microsoft" for work and personal Microsoft accounts.

1. You need your own Entra directory (tenant). A personal Microsoft account has none, and the Entra
   admin center then fails with "Selected user account does not exist in tenant 'Microsoft Services'".
   Create one by signing up at <https://azure.microsoft.com/free> with the same account (a card is
   asked for verification; app registrations are free and nothing is charged unless you create paid
   Azure resources). This makes a directory called "Default Directory".
   Then go to <https://entra.microsoft.com> and check in the account menu (top right, "Switch
   directory") that **Default Directory** is selected. The app still accepts personal and work
   accounts from anywhere (the supported account types below decide that, not the directory).
2. **Applications → App registrations → New registration**:
   - name **Ruum**;
   - supported account types: **Accounts in any organizational directory and personal Microsoft
     accounts**;
   - redirect URI: platform **Web**, `https://auth.ruum.space/ui/login/login/externalidp/callback`.
3. In the new app, **Authentication → Add URI** for the other five redirect URIs of step 4.3 above.
4. **Certificates & secrets → New client secret**, description `ruum-zitadel`, expiry 24 months.
   Copy the **Value** (not the secret ID) into `MS_OAUTH_CLIENT_SECRET`. Note the expiry date in
   your calendar; the agents also record it.
5. From **Overview**, copy the **Application (client) ID** into `MS_OAUTH_CLIENT_ID`.
6. **API permissions**: the default `User.Read` is enough; `openid`, `email` and `profile` are added
   by the sign-in itself.

Later, before many organisations use Ruum: Microsoft shows unverified apps with a warning, and some
organisations block them. "Publisher verification" needs a Microsoft Partner (MPN) ID. This is not
needed for staging.

## 6. Stripe (payments), about 10 minutes

Goal: a Stripe account in **test mode**, so the agents can build plans and checkout without real
money.

1. Go to <https://dashboard.stripe.com/register> and create an account. Choose your country.
2. Stay in **test mode** (the "Test mode" switch, or a sandbox). Do not activate payments yet.
3. **Developers → API keys**: copy the **publishable key** (`pk_test_...`) into
   `STRIPE_PUBLISHABLE_KEY` and the **secret key** (`sk_test_...`) into `STRIPE_SECRET_KEY`.
4. Nothing else for now. The agents create the products, prices and the webhook in test mode.

Later, to take real payments, you must activate the account: the business (company or sole
trader), a bank account, an address and an ID check. EU VAT for digital services (Stripe Tax and
the EU OSS scheme) is a question for that moment; the agents will prepare it when step "plans and
billing" comes.

## When you are done

Tell the agents "secrets are in place" (and which steps you skipped). They check the file, create
nothing that costs money until you say "start staging", and list in the next digest what they set
up.

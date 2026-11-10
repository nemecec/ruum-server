# Estonian eID sign-in for Ruum through Zitadel: provider research

Date of research: 2026-11-11. Only public web pages were read. No company was contacted and nothing was signed up for.
Scope from the product owner: exactly three methods (ID card, Mobiil-ID, Smart-ID), ranked by how well one provider offers all three through one OpenID Connect (OIDC) or SAML integration. Other EU eIDs (eIDAS) are a nice-to-have and are only mentioned briefly.
Design reference: `ruum-server/docs/DESIGN.md` section 4.2 (Zitadel is the identity broker; third-party sign-in is upstream of the Ruum issuer; link identities only by an explicit action of the signed-in user, or by a verified email from a trusted provider).

## 1. Short answer

- Zitadel can use any provider that speaks standard OIDC as a "Generic OIDC" identity provider. Two providers offer ID card, Mobiil-ID and Smart-ID behind one OIDC integration: **Authentigate (run by SK ID Solutions, the company that issues Smart-ID and Mobile-ID)** and **eeID (run by the Estonian Internet Foundation, EIF)**. Neither offers SAML.
- Recommendation: start with **Authentigate**. It gives a pairwise subject identifier (so Ruum does not have to hold the personal identification code), it comes from the issuer itself, and the price is low (EUR 29 per month minimum). Keep **eeID** as the fast and cheap alternative: it has self-service test access and no monthly fee, but its `sub` is the personal identification code in clear and its terms allow processing outside the EEA.
- The Estonian state service TARA is not an option for these three methods: for private companies it offers only cross-border eIDAS.
- Going to the underlying SK APIs directly (Smart-ID and Mobile-ID relying-party APIs, Web eID for the ID card) is not OIDC. Ruum would have to write and run its own OIDC bridge. Not worth it at low volume.

## 2. Comparison table

Ranking is by "all three Estonian methods through one OIDC or SAML integration". Prices are in EUR, VAT excluded unless said otherwise.

| Rank | Provider | Protocol | ID card | Mobiil-ID | Smart-ID | Other countries | Published price | Contract and onboarding | Test environment | Data returned |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | **Authentigate** (SK ID Solutions) | OIDC (code flow, PKCE S256 required). No SAML. | Yes (Estonia) | Yes (EE, LT) | Yes (EE, LV, LT, BE) | LV: eParaksts mobile. No general eIDAS. | Monthly tiers (from 01.06.2025): Starter 100 tx, EUR 29/month, 0.29 per extra tx; Small 500 tx, EUR 49, 0.098; Medium 2,000 tx, EUR 159, 0.0795; Large 5,000 tx, EUR 349, 0.0698; above 7,500 custom. One "transaction" = one Authentigate request, whatever the method. | Request access in the SK e-Service Portal, pick a plan, sign an agreement. Needs a company contract. Client registration needs redirect URIs and the allowed server IP addresses. Client auth: Basic or `private_key_jwt`. | Demo issuer `oidc.demo.sk.ee`; live issuer `id.authentigate.eu`. | `sub` is a **pairwise** identifier. Personal code, name, birth date, age and "over/under N" are only returned if requested by scope. |
| 2 | **eeID** (Estonian Internet Foundation) | OIDC (code flow, PKCE S256 supported, Ory Hydra based). No SAML mentioned. | Yes | Yes | Yes (and Smart-ID+) | eIDAS, Latvia, Lithuania, Belgium, Czechia, Portugal, Sweden (Freja+), Veriff, passkeys. | EUR 0.08 per authentication with ID card, Mobiil-ID or Smart-ID; EUR 0.01 per passkey authentication; EUR 0.10 per document identification. Prepaid. No package or monthly fee. | Self-service: create an account, create a service, EIF approves it, you get client ID and secret. Terms: the client must be a legal entity or other legal person. Not clear if foreign companies are accepted. | `test-auth.eeid.ee`; test authentications are free; Mobile-ID and Smart-ID test accounts from the providers' public demo systems. | `sub` = country code plus **personal identification code in clear** (example `EE60001019906`). Also given name, family name, birth date, `amr`, `acr`. Optional phone (Mobiil-ID) and email scopes. |
| 3 | **Signicat** (owns Dokobit since 2024) | OIDC and SAML 2.0 documented for Mobile-ID. For Smart-ID and ID card I could not confirm the protocol from public pages. | Listed | Listed | Listed | Very wide (Nordics, Benelux, DACH and more). | Not published. "Setup, subscription and transaction fees", volume discounts. Prices only in a dashboard behind a free account or by quote. | Sales contract, enterprise style. | Free sandbox account. | Not confirmed in public pages. |
| 4 | **eID Easy** | OIDC (OAuth 2.0 based "IDaaS"). SAML not mentioned. | Not confirmed in the pages I could read | Not confirmed | Not confirmed | Many; also EU digital identity wallet. | Not published. Bundles by region or all methods; each method can have its own transaction fee. Quote only. | Sales contact. | A "free demo" button; no details. | "Verified identity claims"; claims not listed. |
| 5 | **Dokobit Identity Gateway** | Redirect-based gateway with its own REST API. The pages do not say OIDC or SAML. Would need an adapter for Zitadel. | Yes | Yes | Yes (Smart-ID+, QR and same device) | LV, LT, FI, DK, IS, PL, BE, Estonian e-Residency | Last published price list I found is **valid until 2019** (EUR 50 per month with 700 transactions, EUR 300 set-up). Treat as outdated. | Dokobit Terms of Service and a data processing agreement; now under Signicat. | Sandbox `id-sandbox.dokobit.com`. | Not specified on the pages read. |
| 6 | **SK relying-party APIs directly** (Smart-ID RP API v3, Mobile-ID REST API, Web eID for ID card) | Not OIDC. REST APIs and a browser/smart-card flow. Ruum would build its own OIDC bridge. | Via Web eID (open-source Java validation library; no fee) | Yes | Yes | Smart-ID in LT, LV; Mobile-ID in LT | Smart-ID (from 01.06.2026): tier I 550 tx minimum, EUR 0.109 per tx, EUR 60 minimum per month; tier III 10,000 tx, 0.079, EUR 790. Mobile-ID: tier I 300 tx, 0.109, EUR 33 minimum per month. | Separate contracts for Smart-ID and for Mobile-ID. | Public demo environments and test accounts. | Certificate with personal code and name. |
| n/a | **TARA** (state, RIA) | OIDC | Public sector only | Public sector only | Public sector only | eIDAS for private sector (production currently BE, PT, SE) | Not published on the pages read | Application signed digitally; service is meant for government-sector bodies | Demo `tara-test.ria.ee` | n/a |
| n/a | **Idura** (formerly Criipto) | OIDC, SAML | Not listed | Not listed | Not listed | Nordics, NL, DE, UK | Monthly subscription plus the eID provider's fees; no figures read | Free test account | Yes | Estonian methods do not appear on the product page |
| n/a | **Scrive eID Hub** | REST API or OIDC | Not confirmed | Not confirmed | Not confirmed | "195 countries" | Quote only | Sales | Not stated | Not stated |

Notes on the table:

- Authentigate and eeID are the only two where the public pages show all three methods plus OIDC.
- Authentigate says it counts "transactions made with any of the available eID tools" together, so ID card, Mobiil-ID and Smart-ID share one plan. The wording of the price table is a little unclear (whether the monthly fee includes the minimum transactions). Ask SK.
- The SK price list says prices exclude VAT. eeID's page does not say whether EUR 0.08 includes VAT.
- Third-party blog posts (for example from Didit) repeat SK prices and make claims about TARA. I did not rely on them.

## 3. Rough cost per month (VAT excluded, my own arithmetic from the published prices)

| Logins per month | eeID (0.08 each) | Authentigate | Direct SK APIs (Smart-ID only, Mobile-ID extra) |
|---|---|---|---|
| 50 | EUR 4 | EUR 29 (Starter minimum) | EUR 60 (Smart-ID minimum) plus EUR 33 (Mobile-ID minimum) |
| 1,000 | EUR 80 | about EUR 100 (Small tier) | about EUR 109 for Smart-ID alone |
| 10,000 | EUR 800 | about EUR 700 (Large tier) | EUR 790 for Smart-ID alone |

Conclusion: at the very start (almost no users) eeID is the cheapest; at about 1,000 logins per month all options cost about EUR 100; money is not the deciding factor.

## 4. Details that matter

### Authentigate (SK ID Solutions)
- Announced 15 July 2024; one OIDC integration for Smart-ID, Mobile-ID and ID card in Estonia, plus LV, LT and BE.
- Documentation: `sk-eid.github.io/authentigate-documentation`. Only response type `code`; PKCE `S256` required with basic client authentication; `acr_values` can limit the methods (`mid`, `sid`, `idcard`, and so on).
- Claims are requested by scope: `personal_code`, `personal_code_country`, `given_name`, `family_name`, name, `birthdate`, `age`, `age_over`, `age_under`.
- The ID token `sub` is described as a "pairwise subject unique identifier". The documents say the personal code is returned only if requested. This is the main privacy advantage.
- The client registration needs the IP addresses allowed to call the service. Zitadel's outgoing IP address (token and userinfo calls come from the Zitadel server) must be registered.
- I found no data protection statement in the Authentigate documents. SK publishes general "Principles of Processing Personal Data". Ask SK for the data processing agreement and the processing location.
- Demo access still goes through the SK portal or sales; I could not confirm if the demo needs a signed agreement.

### eeID (Estonian Internet Foundation)
- Documentation: `internetee.github.io/eeID-DOC`. Standard OIDC, discovery document, JWKS, no dynamic client registration.
- Terms of use (approved by the EIF board on 22 May 2026) say: the client is a legal entity or other legal person; availability above 99 percent per year (up to 87 hours of unplanned downtime); EIF is not liable for the third-party eID providers.
- Data protection terms: EIF is an independent controller for its own logs and accounts; the client is an independent controller for its own processing; EIF may use sub-processors; personal data "may be transferred outside the European Economic Area" under adequacy decisions or standard contractual clauses; backups kept 7 days, 4 weeks and 12 months; an end-user account is treated as inactive after five years without a login.
- This conflicts a little with Ruum's "all data stays in the EU" selling point. For a pure login hop this may be acceptable, but it must be checked and written in Ruum's privacy notice.
- `sub` is the country code plus the personal identification code. Ruum (and Zitadel) would see the code in clear.
- The service appears to be new (pricing page dated 4 June 2026, terms in May 2026). Track record is short.

### Signicat, Dokobit, eID Easy
- All are full commercial brokers with quote-based pricing. They are sensible for a larger company that wants Nordic and Benelux eIDs under one contract, but they are more than needed for the three Estonian methods.
- Dokobit's Identity Gateway is the only one of these that I could not tie to OIDC or SAML; the Signicat developer pages list OIDC and SAML 2.0 for Mobile-ID.
- Several Signicat developer pages returned errors when I tried to read them, so the Smart-ID and ID card protocol details for Signicat are unconfirmed.

### TARA
- RIA offers the State Authentication Service to government-sector authorities (RIA page).
- The TARA technical specification does cover private sector clients: they may use only the `openid` scope together with the eIDAS scopes. In production this currently reaches Belgium, Portugal and Sweden. So TARA cannot give a private company Estonian ID card, Mobiil-ID or Smart-ID. It could be a later source of cross-border eIDs, but eeID covers more countries and has no such limit.

### Direct SK APIs
- Smart-ID RP API v3 and Mobile-ID are REST APIs that return a certificate with the personal code and name. The ID card needs the Web eID browser extension with a validation library on the server.
- Ruum would write an OIDC front end for three methods, run it, patch it, and pay two separate minimum fees. SK built Authentigate for exactly this reason.
- Only useful if volume becomes large or if Ruum wants to avoid any middleman.

## 5. What Ruum should store

The problem: the Estonian personal identification code is an 11-digit number that contains the birth date and sex. It is used in many Estonian services as an identifier, so a leak is valuable to an attacker. It is not "special category" data under GDPR Article 9, but Article 87 allows member states to add rules for national identification numbers. I did not verify the Estonian rules. A lawyer should check them before the first real user.

Options, from best to worst for data minimisation:

1. **Store only the provider's subject identifier (`sub`).** This is what Zitadel does by default: it keeps the pair (identity provider, external user id) for each link. With Authentigate the `sub` is pairwise, so Ruum never holds the personal code. Downside: the identifier is only valid for Ruum's client at that provider. If Ruum changes provider, all links break and users must link again (an explicit action, which fits design 4.2).
2. **Store a keyed hash of the personal code (HMAC-SHA256 with a secret key kept in the secret store), in addition to or instead of `sub`.** A plain hash is not safe: the codes follow a known pattern (date of birth plus a serial), so all possible codes can be hashed and compared in minutes. A keyed hash with a key that is outside the database is much safer, and it is stable across providers. The hash allows "this is the same person" across providers and across accounts, and so supports provider changes and duplicate detection. It costs: the code passes through Ruum for a moment (at the first login or link), and the key must be protected and never rotated carelessly (keep key versions).
3. **Store the personal code in clear.** Only with a clear reason (for example a customer who must prove identity to a legal standard). Not recommended as the default.

With eeID the `sub` already contains the code in clear, and Zitadel stores the `sub` in its link table. To avoid that, Ruum would need a small OIDC proxy between Zitadel and eeID that replaces `sub` with the keyed hash. That is extra software to run (several days of work plus operations). With Authentigate this is not needed for the basic case.

Recommended storage rule: the link row keeps provider, `sub` (pairwise or hashed), authentication method (`amr`), assurance level (`acr`) and the time of the last login. Name and birth date are used only to fill the profile display name at first sign-up and are not used as identity keys. The personal code is requested only when needed (for example when migrating providers) and never logged. Also make sure the access logs and Zitadel's debug logs do not print ID tokens.

## 6. Effort in Ruum (rough)

| Work item | Rough effort |
|---|---|
| Contract and test access with the chosen provider | 1 to 3 weeks elapsed time, little work time (an owner of a company must sign) |
| Zitadel: add a Generic OIDC identity provider (issuer URL, client ID and secret, scopes `openid` and any claims, PKCE on, name shown on the login button), set it active, allow external login in the login policy | 0.5 to 1 day |
| Spike: check how Zitadel creates a user from an ID token without an email address (eID gives no verified email; the ID card can give an unverified one). May need a Zitadel Action or a placeholder email, or the "prompt for missing data" screen | 1 to 2 days |
| Linking rules in the Ruum sign-in flow (see below) and tests against the provider's test environment | 2 to 4 days |
| Tenant settings: show "eID" in the "allowed identity providers" per tenant; optional rule "require high assurance" | 1 to 2 days |
| Optional: pseudonymising proxy (only for eeID) | 3 to 5 days plus running costs |
| Register Zitadel's outgoing IP address with Authentigate | Hours |

Account linking rules, based on design 4.2:

- eID gives **no verified email**. The ID card certificate can show an email address but eeID and TARA mark it as not verified. The "verified email from a trusted provider" rule in 4.2 does not apply. So **never auto-link an eID login to an existing account by email or name.** In Zitadel, switch off the automatic linking by email or username for this provider (the "Account linking allowed" option is an option to review carefully, because Zitadel's docs describe matching on username or email).
- Link an eID to an existing account only when the user is already signed in (settings: "Add eID sign-in"). Zitadel's own page on account linking does not describe this manual flow, so Ruum may need to use the Zitadel API for it (the API has calls to add an identity provider link to a user).
- A first-time eID login with no matching link creates a new account. The user is asked to add a verified email afterwards (needed for notifications and recovery). If someone later signs in with Google and the same email, the user must merge by an explicit action.
- If the same person ends up with two accounts (eID account and email account), offer a signed-in "merge" step; it must prove both sign-ins in one session.
- A person who loses access to their eID (new phone, lost card) needs a recovery path that does not weaken the rest. Passkeys or a second linked identity solve this.
- Name differences: the name from the eID is verified and may be shown as "verified name", but it must be editable as a display name.

## 7. Other EU eIDs later

- eeID already lists eIDAS, Latvia, Lithuania, Belgium, Czechia, Portugal and Sweden (Freja+) on its pricing and documentation pages, and it supports passkeys. This is the simplest later step because it needs only another scope or button on the same OIDC connection.
- Authentigate lists LV, LT and BE (not general eIDAS).
- TARA for private companies: eIDAS only, currently BE, PT, SE in production.
- If a customer asks for a country that none of these covers, Signicat is the large-coverage option, at a quote price.

## 8. Open questions

1. Does Authentigate require a company with a registered office in EE, LV or LT? Its price list says "applies to e-service providers from Estonia, Latvia and Lithuania". Is Ruum's company in one of these countries? If not, a custom agreement is needed. Same question for eeID (the terms say only "legal entity or other legal person").
2. Authentigate pricing: does the minimum monthly fee include the minimum number of transactions (for example Small: EUR 49 covers 500)? Is there a cap on test/demo use?
3. Where is the data processed (country, sub-processors) for Authentigate? SK is an Estonian company, but I found no data protection statement in the Authentigate documents. For eeID, the terms allow processing outside the EEA.
4. Zitadel: how does it create a user when the provider gives no email? Which claims can be mapped to username and display name (Zitadel Actions)? Does Zitadel keep the raw claims (including the personal code) anywhere besides the link table?
5. Can Zitadel register as an Authentigate client (IP allow-list, PKCE S256, client secret Basic authentication)? Standard OIDC says yes, but it must be tried in the demo environment.
6. Legal: rules for national identification numbers in Estonia (GDPR Article 87), legal basis for processing the code if Ruum asks for it, and Ruum's privacy notice.
7. Does Ruum want to show eID assurance level ("high") to tenant admins, for example to require it for administrators?
8. Is eID sign-in for natural persons only? Business use (company representatives, e-Residents) may need other flows (e-Residents also have an ID card and can use Smart-ID; Dokobit lists e-Residency).
9. Dokobit: current price list and OIDC support (the only price list found is from 2019). Signicat: protocol for Smart-ID and ID card. eID Easy: whether Estonian methods are included in a bundle. These are only relevant if both top choices fail.
10. Business continuity: eeID is new and has a 99 percent availability promise; ask Authentigate for its SLA. Plan a fallback: because both use plain OIDC, a second provider can be added as another Zitadel identity provider in under an hour, but only if links are keyed with the keyed hash (option 2 above) or if users re-link.

## 9. Sources

SK ID Solutions and Authentigate
- Price list (Smart-ID, Mobile-ID, broker, Authentigate): https://www.skidsolutions.eu/?p=1624
- Authentigate announcement page (Smart-ID site): https://www.smart-id.com/?p=20175
- SK product page for Authentigate: https://www.skidsolutions.eu/services/authentigate/
- Authentigate documentation start: https://sk-eid.github.io/authentigate-documentation
- Authentigate overview (attributes, methods): https://sk-eid.github.io/authentigate-documentation/api/overview.html
- Authentigate API specification (scopes, sub, PKCE, acr values): https://sk-eid.github.io/authentigate-documentation/api/index.html
- Authentigate client registration: https://sk-eid.github.io/authentigate-documentation/api/registration.html
- Authentigate environments: https://sk-eid.github.io/authentigate-documentation/environments.html
- Authentigate how to get access: https://sk-eid.github.io/authentigate-documentation/contact.html
- Smart-ID documentation (RP API v3): https://sk-eid.github.io/smart-id-documentation/
- Smart-ID technical overview (old OIDC note): https://github.com/SK-EID/smart-id-documentation/wiki/Technical-overview
- SK principles of processing personal data: https://www.sk.ee/upload/files/SK-Principles%20of%20Processing%20Personal%20Data%20v3_0.pdf

eeID (Estonian Internet Foundation)
- Service overview: https://eeid.ee/about
- Pricing: https://eeid.ee/pricing
- Pricing news (4 June 2026): https://www.internet.ee/eif/news-archive/eeid-introduces-new-pricing-and-updates-to-strengthen-digital-identity-verification
- Technical documentation: https://internetee.github.io/eeID-DOC
- Information page for users (data processed): https://www.internet.ee/eeid/eeid-for-users
- Terms of use (PDF): https://meedia.internet.ee/files/eeID%20Terms%20of%20Use.docx.pdf
- Annex 2, data protection terms (PDF): https://meedia.internet.ee/files/Annex%202_Data%20protection%20terms_eeID.docx.docx.pdf

State service TARA
- RIA authentication services: https://www.ria.ee/en/state-information-system/electronic-identity-eid-and-trust-services/authentication-services
- TARA technical specification: https://e-gov.github.io/TARA-Doku/TechnicalSpecification

Others
- Dokobit Identity Gateway page: https://craft.dokobit.com/solutions/identification-api
- Dokobit identity pricing (valid until 2019): https://www.dokobit.com/docs/Dokobit-Identity-Pricing.pdf
- Dokobit developer guide (Smart-ID, Mobile-ID): https://support.dokobit.com/category/537-developer-guide
- Signicat pricing page: https://www.signicat.com/pricing
- Signicat Mobiil-ID page (found in search; page itself did not load for me): https://developer.signicat.com/identity-methods/mobiilid/
- eID Easy documentation: https://docs.eideasy.com/
- eID Easy pricing: https://www.eideasy.com/pricing
- Idura products: https://idura.eu/products
- Scrive eID Hub pricing: https://www.scrive.com/pricing/eid-hub
- Web eID (ID card in the browser): https://github.com/web-eid

Zitadel
- Generic OIDC identity provider: https://zitadel.com/docs/guides/integrate/identity-providers/generic-oidc
- Account linking: https://zitadel.com/docs/concepts/features/account-linking
- Identity providers overview: https://zitadel.com/docs/guides/integrate/identity-providers

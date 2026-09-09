# The phone and amux.sh

What the iPhone app asks of the account service, and where every value the
app needs to ask it lives. The app is the only client described here; the
terminal client and the web app talk to the same service through their own
paths, and nothing below is a general description of the API.

Two rules run through all of it:

- **The phone holds no secret.** There is no client secret, no App Store
  shared secret and no payment-provider key in this repository or in the
  shipped binary. Sign-in is an authorization code with PKCE, and the only
  thing the app ever holds is a token issued to it.
- **The cloud decides what an account may do.** The App Store can say this
  Apple Account paid; only amux.sh can say this amux account is entitled.
  Every screen reads the entitlement back from the cloud rather than
  inferring it from a purchase.

## Endpoints the app uses

The base is `https://amux.sh`. The client is registered as `mobile` with the
one redirect `amux://callback` and no secret. The scopes asked for are
`openid profile email offline_access api`; `offline_access` is what makes
sign-in happen once rather than hourly.

| Path | Method | What it is for |
| --- | --- | --- |
| `/connect/authorize` | browser | Sign-in, opened in the system browser with a PKCE challenge and a state. |
| `/connect/token` | POST | Redeeming the code, and later refreshing. Refresh tokens rotate: the one that comes back replaces the one just spent. |
| `/connect/userinfo` | GET | Who signed in — `sub`, `email`, `name`. `sub` is the account identifier everything else is asked for by. |
| `/api/graphql` | POST | The entitlement read. See below. |
| `/api/connect` | GET | A relay credential for this account, good for the hour. Answers `host`, `port`, `token`, `expires_at`. |
| `/api/purchases` | POST | A purchase the App Store signed. See below. |
| `/api/account` | DELETE | Deletes the account. `409` when a subscription is still set to renew. |
| `/api/billing/stripe/portal` | POST | A one-time link to stop a web subscription. Asked for only when a deletion is actually blocked, because the link expires. |
| `/api/reports` | POST | A debug report bundle, `multipart/form-data`, one section per file. Debug builds only. |

Every one but `/connect/authorize` and `/connect/token` is sent with
`Authorization: Bearer <access token>`, refreshed a minute before it expires.

## The entitlement read

One query, against `/api/graphql`:

```graphql
{ me { subscription { status provider willRenew entitledUntil } } }
```

No subscription at all is `none`. Otherwise `entitledUntil` decides: past it
the entitlement is *lapsed* and says when it ended; before it the entitlement
is *active*, renewing on `entitledUntil` when `willRenew` and on no date when
it does not — a subscription that was cancelled but has not run out is still
paid for. `provider` says where it was bought, which is the one thing a screen
shows about it: `REVENUE_CAT` is the App Store and anything else is the web.

This read is the single source of truth for what an account may do. A
subscription bought on the web through the CLI and one bought in the App Store
on this phone arrive through exactly the same answer.

## A purchase reaching the cloud

`POST /api/purchases`, `application/json`, one field:

```json
{"signed_transaction": "<the App Store's JWS>"}
```

The signed transaction is carried whole and unread. The app does not parse it,
does not trust its contents and does not know which billing system reconciles
it on the other side — that is the cloud's business, and an app that knew would
be a second place for it to change.

The order matters and is the point of the whole path:

1. The App Store signs a purchase. The transaction is **not** finished.
2. The signed transaction is posted here. `200` and `202` both mean taken.
3. Only then is the transaction finished with the App Store.
4. The entitlement is read back from `/api/graphql`. Nothing is assumed from
   the store.

A transaction finished before step 2 succeeds is one the App Store will never
offer this app again, and a subscription somebody paid for would exist nowhere
but on their bank statement. Because the transaction survives, an unconfirmed
purchase is temporary: the paywall offers Retry, and the next launch sends
everything the store is still holding without anybody pressing anything.
Purchases approved later — a parent answering Ask to Buy, a bank's second
factor — arrive on StoreKit's updates and take the same road.

What the app does with each answer:

| Answer | What the person sees |
| --- | --- |
| `200`, `202` | The entitlement is read back; the paywall says subscribed. |
| `401` | Unconfirmed, and read as unreachable: the session is renewed on the next launch, which then sends the purchase again. |
| `403 payment_required` | Unconfirmed and refused, in the words the gate uses. |
| `422` | Unconfirmed and refused, in the cloud's own words. |
| no answer at all | Unconfirmed and unreachable: paid for, kept, and tried again. |

## The `payment_required` rule

`403` with `{"error":"payment_required"}` means the account has nothing bought.
It is not an error to report as a failure: it is the gate the home screen
already draws, and the app says *this account has no subscription* wherever it
comes back — the connect token, a purchase, anything else. Any other `403`
carries the service's own `error_description` and is shown as it is.

## Where each value lives

**In this repository, committed and public** — none of it is a secret and all
of it is visible in any copy of the app anyway:

- The App Store product identifiers, `amux_pro_monthly` and `amux_pro_yearly`,
  in `ios/Packages/AmuxCore/Sources/AmuxCore/Store.swift`.
- The amux.sh base URL, the client identifier `mobile`, the redirect
  `amux://callback` and the scopes, in `CloudEndpoint.production` in
  `ios/Packages/AmuxCore/Sources/AmuxCore/AmuxCloud.swift`. Every other URL
  the app offers — support, the account page — is derived from that base, so a
  build pointed elsewhere cannot offer the production one's pages.
- The subscription's own name as the cloud reports it. There is no entitlement
  identifier in this app: what an account may do is the shape of the
  `subscription` answer above, not a string matched against a constant.

**In the amuxcloud repository, encrypted** (that service is a separate .NET
repository; it is what issues connect tokens and what verifies purchases):

- The App Store server credentials and the payment provider's keys and
  webhook secrets, in that repository's SOPS-encrypted secrets. None of them
  ever appear here, and the app has no use for them: it carries a signature
  Apple made and nothing else.
- The QA allowlist, in that repository's `appsettings`. It is configuration
  rather than a secret, but it names people, so it lives there.

**Nowhere in this repository, at all:** the addresses of the QA accounts. Not
in a script, a document, a fixture, a golden, a journey record, an evidence
file or a transcript. A recipe that needs one is given it at the moment it
runs, from the environment or from an operator's own untracked file.

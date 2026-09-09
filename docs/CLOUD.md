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

## The access read

One query, against `/api/graphql`:

```graphql
{ me { access {
    pro
    until
    grant {
      __typename
      ... on Purchased { status provider willRenew entitledUntil trialEndsAt }
      ... on Granted { reason }
    }
} } }
```

`pro` is the only thing anything gates on, and it is never null. `until` is
when access runs out, absent when it does not run out at all. `grant` explains
where the access came from, and an account can be entitled without ever having
bought anything — a gift, a beta, an employee, a referral, an administrator's
grant — so the explanation is a choice between two shapes rather than a
subscription that might be missing.

`Purchased` says what was bought and how it is going: `provider` is the one
thing a screen shows about the source (`REVENUE_CAT` is the App Store,
anything else is the web), `entitledUntil` is when the paid period runs out,
and `willRenew` is whether it will be charged again. That date is not always
`until`: somebody who is both paying and holding an open-ended grant keeps
access after the billing stops, and the two fields answer the two questions
separately. `status` distinguishes a subscription that has ended from one that
has not, which matters in the seconds between a purchase landing and the
access behind it being projected — the paid period is live and `pro` is not
yet true, and the screen for that says Pro is switching on rather than
offering a subscription the person already has.

`Granted` says why access was given. A client has to handle it: it is a state
with a name, not a subscription that failed to arrive.

**This read and the relay's own gate answer from the same place.** `pro` and
`GET /api/connect` are one call into the account service, so the phone and the
relay cannot come to different conclusions about the same account. What this
read is *not* is a report of how somebody pays; that lives inside `grant`,
underneath the answer, precisely so nothing gates on it by mistake. A
subscription bought on the web through the CLI, one bought in the App Store on
this phone, and access given by hand all arrive through the same `pro`.

## A purchase reaching the cloud

`POST /api/purchases`, `application/json`, one field:

```json
{"signed_transaction": "<the App Store's JWS>"}
```

The signed transaction is carried whole and unread. The app does not parse it,
does not trust its contents and does not know which billing system reconciles
it on the other side — that is the cloud's business, and an app that knew would
be a second place for it to change.

On the other side the transaction is handed to the billing provider as a
receipt against this account, and the account's subscription is then refetched
from that provider and projected — the same projection the provider's own
webhook performs, run at once rather than whenever that webhook arrives. So a
purchase made on this phone and one the webhook reports later are the same
thing recorded once, and the entitlement read answers the same either way. A
`200` carries that subscription; a `202` means the cloud has the transaction
but the provider has not turned it into anything yet.

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
| `502` | Unconfirmed and refused, in the cloud's own words: it could not reach the billing provider, so nothing was recorded. |
| no answer at all | Unconfirmed and unreachable: paid for, kept, and tried again. |

## Which read answers which question

The app asks two questions and never mixes them up.

*May this account act?* is `pro`, and only `pro`. Not the `tier` claim in the
access token, which is a copy of the same answer that goes stale between token
issues; not the presence of a billing record, which many entitled accounts do
not have; and not a date compared against the phone's clock. When access ends
is the account service's answer, not a sum this phone does.

*Why does it have it, and where would somebody change it?* is `grant`, and
only `grant`. It is what the paywall and the Settings row read to name the
store a subscription was bought in and offer to manage it. An account whose
access was given is a case those screens handle in words of its own: they name
no store, and they do not offer to manage a subscription that does not exist.

The app got this wrong before, and it is worth saying how, because the shape
of the read is what prevents it. It used to ask about the subscription record
and treat its absence as the absence of access — so an account entitled by a
gift, as every complimentary, employee and beta account is, was shown a
paywall while the relay was already letting it in.

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
- No entitlement identifier at all. What an account may do is `pro` in the
  access read above, and nothing in the app is matched against a constant to
  decide it.

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
runs, from the environment or from an operator's own untracked file — see
below.

## QA recipes

These are evidence a person runs on this Mac. They are not tests, they never
run in CI, and nothing gates on them: they reach the production account
service with a real account's credentials, and a red one is a conversation
rather than a build failure. They are deliberately absent from the iOS
verification list.

- `wt run qa-cloud-signin` — signs a QA account into `https://amux.sh` with
  no app at all, performing the same authorization-code sign-in with PKCE the
  phone performs, as the same `mobile` client with the same redirect and
  scopes. It then asks the three questions the app asks: who the account is,
  what it is entitled to, and whether the relay will issue it a credential.
  What it proves is that the contract above is the contract the live service
  actually keeps — a sign-in that works in a simulator against a double proves
  nothing about the production one.

- `wt run qa-sandbox-purchase` — carries a real App Store sandbox purchase
  from a physical iPhone to the account service and reads back what the
  account may then do. A sandbox transaction exists in exactly one place: a
  phone signed into a sandbox Apple Account, running a development-signed
  build. Apple's engineers say so plainly — sandbox sign-in is not supported
  on the Simulator — and `ios/Amux/Amux.storekit` is a StoreKit Testing
  configuration, whose transactions are signed by the local test certificate
  and are not sandbox transactions. So this recipe never runs on a simulator
  and never invents a transaction. What it proves is the one thing no
  simulator run can: that a purchase the real App Store signed reaches
  amux.sh, is recognised on the other side, and turns the relay's
  `payment_required` refusal into a credential.

  `--preflight` reports what is present and missing on this Mac and exits 0,
  touching neither StoreKit, the store nor amux.sh. Without it, anything
  missing is named and the run exits non-zero. Four facts are checked here —
  the account's address, its keychain password, a phone reachable over `xcrun
  devicectl`, and a Team ID and bundle id in `ios/Signing.local.xcconfig`, an
  untracked file `.gitignore` covers because this repository builds
  simulator-only and holds no signing identity. Two more cannot be checked
  from a Mac at all and are printed as facts to confirm: that the bundle id
  carries both subscription products in App Store Connect and is known to the
  billing provider, and that the phone's sandbox Apple Account is this
  account. `--confirmed` says they are true; nothing here asserts them for
  you.

  A full run generates the project, builds and installs a development-signed
  build on the phone, names the purchase to make, and then watches amux.sh as
  that account — the entitlement read and `GET /api/connect`, asked
  separately because they are answered from different places — until a
  credential is issued or a bound elapses, saying which in words.
  `--transaction-file PATH` is the other road to the same two answers: a
  transaction a phone already signed, posted through `POST /api/purchases` as
  this account. That file is never committed.

  **Its account.** The device QA account and only that: the one
  `AMUX_QA_DEVICE_EMAIL` names, in the environment or in
  `.autopilot/qa-account.env`. The end-to-end account the sign-in recipe uses
  is refused even when that variable names it, because it is not a sandbox
  account. There is no built-in address, so until an operator sets the
  variable the honest preflight result is that the address source is missing,
  naming the variable and the file.

  **It is a person's act, before every release.** No credential turns this
  into something a machine performs unattended. Installing a
  development-signed build on a phone, signing that phone into a sandbox
  Apple Account and tapping through a purchase is a human sequence, and the
  Simulator cannot stand in for it at any price. So the App Store route to an
  entitlement — a purchase the store signed, recognised at amux.sh, turning
  `payment_required` into a credential — is unproven on a machine that has
  only run the simulator, and stays unproven until somebody runs this recipe
  on a phone. Run it before each release, and read what it printed rather
  than ticking a box: what a build proved last time says nothing about the
  products, the bundle id or the provider configuration this one ships with.
  The web route to the same entitlement is a different question and is
  covered by `wt run qa-cloud-signin` and `wt run qa-live-journey`.

- `wt run qa-live-journey` — the whole product once, against the real one. It
  signs the QA account into `https://amux.sh`, hands the simulator app that
  session and nothing else, and then stands back: the app asks the account
  service who the account is and what it may do, asks it for a relay
  credential, and dials the relay that credential names. On the other side is
  this checkout's own daemon — its socket, state and identity under
  `.wt/amux`, started by `wt run daemon` — on a profile the recipe creates for
  the run and destroys afterwards, signed in as the same account by completing
  the CLI's device-code flow in the browser session it already holds. The
  phone then trusts that machine by the invitation it printed, opens a
  conversation with a real Claude session running on it, asks one question and
  reads the answer back.

  What it proves that nothing else does: the production handshake. Every other
  journey runs against a relay started beside it with credentials a harness
  minted, so the audience, port and client identifier the relay compares with
  its own configuration, and the RSA signature it validates, are never
  exercised until here. A mismatch there fails silently — the phone simply
  never arrives — so a failure is reported with what the phone's runtime and
  the daemon logged rather than as a timeout.

  It reaches a real account and spends real money on a real agent, so it is a
  person's act and never a gate. The entitlement it proves is the web one: an
  account entitled through amux.sh reaching a machine and an agent. It says
  nothing about the App Store route, which needs a phone in somebody's hand.
  If `/api/connect` answers `403 payment_required`, the dedicated account's
  entitlement has lapsed — it was granted once by hand through the QA coupon
  path and nothing renews it. That is an operator's to restore; the recipe
  says so and stops, because an entitlement row written by hand is a
  projection the next provider sync overwrites.

**The accounts.** Two variables name two accounts — `AMUX_QA_EMAIL` for the
sign-in and the live journey, `AMUX_QA_DEVICE_EMAIL` for the sandbox purchase
— because only one of them is a sandbox account. When the
environment already sets a variable, that value is used and nothing overwrites
it; otherwise the recipe reads `.autopilot/qa-account.env` at the repository
root, an operator-written file that sets those variables and nothing else.
That directory is untracked, which is the point: this repository is public.
There is no built-in address to fall back to.

**The password.** Read from this Mac's login keychain at the moment it is
needed, with `security find-generic-password -s amuxcloud-qa -a "<the
address>" -w`, and never printed, logged or written anywhere. Add one with
`security add-generic-password -s amuxcloud-qa -a "<the address>" -w`. A
password is never written down beside the address it belongs to.

**What is never printed.** An address appears only masked; the password, the
authorization code and every token are used and dropped; the account
identifier is not printed at all, because it finds a person as well as an
address does. No address is written in this document, in a script, in an
example, in a golden, in a journey record or in an evidence file.

When there is no address, no keychain entry, or the login form cannot be
driven, a recipe says which of those it is — naming the variable and the file
when the address is what is missing — and exits non-zero. Neither ever passes
quietly on work it did not do.

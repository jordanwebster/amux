#!/usr/bin/env python3
"""Sign a QA account into the real amux.sh the way the phone does, then read
what that account may do.

This is evidence a person runs on this Mac, not a test. Nothing here is
mocked: it performs the same authorization-code sign-in with PKCE the app
performs, against the same client, and then asks the three questions the app
asks about an account — who it is, what it is entitled to, and whether the
relay will issue it a credential.

Nothing that identifies the account ever reaches the output. The address is
masked, and the password, the authorization code and every token are read,
used and dropped.
"""

from pathlib import Path
from urllib import error, parse, request
import base64
import hashlib
import html
import http.cookiejar
import json
import os
import re
import secrets
import subprocess
import sys

BASE = "https://amux.sh"
CLIENT_ID = "mobile"
CALLBACK = "amux://callback"
SCOPES = ["openid", "profile", "email", "offline_access", "api"]
# Where an address lives when the environment does not carry one. Outside the
# tracked tree on purpose: this repository is public and the address names an
# account with QA privileges.
ADDRESS_FILE = Path(".autopilot/qa-account.env")
ADDRESS_VARIABLE = "AMUX_QA_EMAIL"
KEYCHAIN_SERVICE = "amuxcloud-qa"


def fail(why: str) -> None:
    """Says what is missing and stops. A recipe that cannot sign in must never
    read as one that signed in and found nothing."""
    print(f"qa-cloud-signin: {why}", file=sys.stderr)
    raise SystemExit(1)


def masked(address: str) -> str:
    """The address as it may be written down: enough to tell two QA accounts
    apart in a transcript, never enough to be one."""
    name, _, domain = address.partition("@")
    return f"{name[:1]}***@***{domain[-3:]}" if domain else "***"


def address() -> str:
    """The account to sign in as.

    The environment wins, so one run can name another account without editing
    anything. There is no built-in address: an account this recipe reaches by
    default would be an account committed to a public repository."""
    from_environment = os.environ.get(ADDRESS_VARIABLE, "").strip()
    if from_environment:
        return from_environment
    if not ADDRESS_FILE.exists():
        fail(
            f"no account to sign in as. Set {ADDRESS_VARIABLE} in the "
            f"environment, or create {ADDRESS_FILE} setting "
            f"{ADDRESS_VARIABLE} and nothing else. That file is outside the "
            "tracked tree and must stay there."
        )
    for line in ADDRESS_FILE.read_text().splitlines():
        line = line.strip()
        if line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        if key.strip() == ADDRESS_VARIABLE:
            found = value.strip().strip('"').strip("'")
            if found:
                return found
    fail(f"{ADDRESS_FILE} does not set {ADDRESS_VARIABLE}")


def password(for_address: str) -> str:
    """Read from the login keychain at the moment it is needed, and never
    written anywhere."""
    found = subprocess.run(
        ["security", "find-generic-password", "-s", KEYCHAIN_SERVICE,
         "-a", for_address, "-w"],
        capture_output=True, text=True, timeout=60)
    if found.returncode != 0 or not found.stdout.strip():
        fail(
            f"the login keychain has no password for {masked(for_address)} "
            f"under service {KEYCHAIN_SERVICE}. Add it with: security "
            f'add-generic-password -s {KEYCHAIN_SERVICE} -a "${ADDRESS_VARIABLE}" -w'
        )
    return found.stdout.rstrip("\n")


class Browser:
    """A cookie jar and a redirect that stops rather than following.

    The sign-in ends at `amux://callback`, which no HTTP client can follow, so
    every redirect is read here and the hand-off is recognised by its scheme —
    exactly what the app's own web session does."""

    def __init__(self) -> None:
        self.jar = http.cookiejar.CookieJar()
        self.opener = request.build_opener(
            request.HTTPCookieProcessor(self.jar), NoRedirect())

    def get(self, url: str) -> tuple[int, str, str]:
        return self.send(request.Request(url, method="GET"))

    def post(self, url: str, fields: dict[str, str]) -> tuple[int, str, str]:
        body = parse.urlencode(fields).encode()
        return self.send(request.Request(
            url, data=body, method="POST",
            headers={"Content-Type": "application/x-www-form-urlencoded"}))

    def send(self, prepared) -> tuple[int, str, str]:
        prepared.add_header("User-Agent", "amux-qa-cloud-signin")
        try:
            answer = self.opener.open(prepared, timeout=60)
        except error.HTTPError as refused:
            answer = refused
        except error.URLError as unreachable:
            fail(f"{BASE} could not be reached: {unreachable.reason}")
        body = answer.read().decode("utf-8", "replace")
        return answer.status, answer.headers.get("Location", ""), body


class NoRedirect(request.HTTPRedirectHandler):
    def redirect_request(self, *arguments):
        return None


def verifier_and_challenge() -> tuple[str, str]:
    verifier = base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("=")
    digest = hashlib.sha256(verifier.encode()).digest()
    return verifier, base64.urlsafe_b64encode(digest).decode().rstrip("=")


def authorize_url(challenge: str, state: str) -> str:
    return f"{BASE}/connect/authorize?" + parse.urlencode({
        "client_id": CLIENT_ID,
        "response_type": "code",
        "redirect_uri": CALLBACK,
        "scope": " ".join(SCOPES),
        "code_challenge": challenge,
        "code_challenge_method": "S256",
        "state": state,
    })


def form_fields(page: str) -> dict[str, str]:
    """Every hidden input on the login form, the antiforgery token among
    them. Read off the page rather than guessed, because a token this recipe
    invented would be refused."""
    fields = {}
    for tag in re.findall(r"<input[^>]*>", page, re.IGNORECASE):
        if 'type="hidden"' not in tag.lower():
            continue
        name = re.search(r'name="([^"]+)"', tag)
        value = re.search(r'value="([^"]*)"', tag)
        if name:
            # Unescaped, because the page is HTML: the return URL comes back
            # with its ampersands as entities, and posting it as written would
            # hand amux.sh a query with one parameter in it.
            fields[name.group(1)] = html.unescape(value.group(1)) if value else ""
    return fields


def sign_in(browser: Browser, who: str, secret: str, challenge: str,
            state: str) -> str:
    """Drives the browser hand-off to its end and answers the code."""
    location = authorize_url(challenge, state)
    seen = 0
    while seen < 12:
        seen += 1
        if location.startswith(CALLBACK.split(":")[0] + ":"):
            returned = parse.parse_qs(parse.urlparse(location).query)
            if "error" in returned:
                fail(f"amux.sh refused the sign-in: {returned['error'][0]}")
            if returned.get("state", [""])[0] != state:
                fail("the callback answered a different sign-in request")
            code = returned.get("code", [""])[0]
            if not code:
                fail("the callback carried no authorization code")
            return code
        status, redirect, body = browser.get(location)
        if status in (301, 302, 303, 307, 308) and redirect:
            location = parse.urljoin(location, redirect)
            continue
        if status != 200:
            fail(f"amux.sh answered {status} where a sign-in page was expected")
        if "Input.Password" not in body:
            # Either the login form has changed shape, or the hand-off went
            # somewhere else entirely — amux.sh's own error page, most often.
            # The page it stopped on is named, because that is the one fact
            # that says which of the two happened.
            fail(
                "the sign-in could not be driven to its end: "
                f"{parse.urlparse(location).path} is not a page with a "
                "password field on it. amux.sh may have changed its login "
                "form, or refused the request that page was reached through."
            )
        fields = form_fields(body)
        fields["Input.Email"] = who
        fields["Input.Password"] = secret
        fields["Input.RememberMe"] = "false"
        status, redirect, body = browser.post(location, fields)
        if status == 200:
            fail(
                "amux.sh would not sign that account in. The address and the "
                "keychain password are what to check; neither is printed."
            )
        if not redirect:
            fail(f"the sign-in form answered {status} and went nowhere")
        location = parse.urljoin(location, redirect)
    fail("the sign-in went round in circles and never reached the callback")


def redeem(browser: Browser, code: str, verifier: str) -> dict:
    status, _, body = browser.post(f"{BASE}/connect/token", {
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": CALLBACK,
        "client_id": CLIENT_ID,
        "code_verifier": verifier,
    })
    if status != 200:
        fail(f"the authorization code was not redeemed: amux.sh answered {status}")
    return json.loads(body)


def ask(browser: Browser, url: str, token: str, method: str = "GET",
        body: str | None = None) -> tuple[int, str]:
    prepared = request.Request(
        url, data=body.encode() if body else None, method=method,
        headers={"Authorization": f"Bearer {token}"}
        | ({"Content-Type": "application/json"} if body else {}))
    return browser.send(prepared)[::2]


def entitlement(answer: str) -> str:
    """The same read the app makes, said in a sentence."""
    try:
        subscription = json.loads(answer)["data"]["me"]["subscription"]
    except (KeyError, TypeError, ValueError):
        return "amux.sh answered in a shape this recipe does not know"
    if not subscription:
        return "no subscription: this account is entitled to nothing"
    where = "the App Store" if subscription.get("provider") == "REVENUE_CAT" else "the web"
    renewal = "renewing" if subscription.get("willRenew") else "not renewing"
    return (f"entitled until {subscription.get('entitledUntil')}, bought on "
            f"{where}, {renewal} (status {subscription.get('status')})")


def main() -> None:
    who = address()
    secret = password(who)
    print(f"signing {masked(who)} in at {BASE} as client {CLIENT_ID}")

    browser = Browser()
    verifier, challenge = verifier_and_challenge()
    state = secrets.token_urlsafe(24)
    code = sign_in(browser, who, secret, challenge, state)
    del secret
    issued = redeem(browser, code, verifier)
    token = issued["access_token"]
    print("signed in: amux.sh issued an access token"
          + (" and a refresh token" if issued.get("refresh_token") else
             " and no refresh token"))

    status, body = ask(browser, f"{BASE}/connect/userinfo", token)
    if status != 200:
        fail(f"amux.sh would not say who this account is: {status}")
    who_it_is = json.loads(body)
    # Whether it is the same account, not which account. The identifier is as
    # good as the address for finding a person, so neither is printed.
    same = who_it_is.get("email", "").lower() == who.lower()
    print("who: amux.sh answers with "
          + ("the same address that signed in" if same
             else "a DIFFERENT address from the one that signed in"))

    status, body = ask(
        browser, f"{BASE}/api/graphql", token, "POST",
        '{"query":"{ me { subscription { status provider willRenew entitledUntil } } }"}')
    if status != 200:
        fail(f"the entitlement read answered {status}")
    print(f"entitlement: {entitlement(body)}")

    status, body = ask(browser, f"{BASE}/api/connect", token)
    if status == 200:
        issued_connect = json.loads(body)
        print(f"connect: a relay credential was issued for "
              f"{issued_connect.get('host')}:{issued_connect.get('port')}, "
              f"expiring {issued_connect.get('expires_at')}")
    elif status == 403 and "payment_required" in body:
        print("connect: refused with payment_required — this account has no "
              "subscription, so the phone would show the second gate")
    else:
        fail(f"the connect endpoint answered {status}, which is neither a "
             f"credential nor the subscription gate")


if __name__ == "__main__":
    main()

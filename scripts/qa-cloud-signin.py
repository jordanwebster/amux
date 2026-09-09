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
import json
import sys

sys.path.insert(0, str(Path(__file__).parent))
import qa_cloud
from qa_cloud import BASE, CLIENT_ID, fail, masked

qa_cloud.PROGRAM = "qa-cloud-signin"

ADDRESS_VARIABLE = "AMUX_QA_EMAIL"


def address() -> str:
    """The account to sign in as: the environment, then the operator's file."""
    found = qa_cloud.address(ADDRESS_VARIABLE)
    if not found:
        fail(
            f"no account to sign in as. Set {ADDRESS_VARIABLE} in the "
            f"environment, or create {qa_cloud.ADDRESS_FILE} setting "
            f"{ADDRESS_VARIABLE}. That file is outside the tracked tree and "
            "must stay there."
        )
    return found


def main() -> None:
    who = address()
    secret = qa_cloud.password(who)
    print(f"signing {masked(who)} in at {BASE} as client {CLIENT_ID}")

    browser, issued = qa_cloud.signed_in(who, secret)
    del secret
    token = issued["access_token"]
    print("signed in: amux.sh issued an access token"
          + (" and a refresh token" if issued.get("refresh_token") else
             " and no refresh token"))

    print(f"tier: the access token claims {qa_cloud.tier(token)}")

    status, body = qa_cloud.ask(browser, f"{BASE}/connect/userinfo", token)
    if status != 200:
        fail(f"amux.sh would not say who this account is: {status}")
    who_it_is = json.loads(body)
    # Whether it is the same account, not which account. The identifier is as
    # good as the address for finding a person, so neither is printed.
    same = who_it_is.get("email", "").lower() == who.lower()
    print("who: amux.sh answers with "
          + ("the same address that signed in" if same
             else "a DIFFERENT address from the one that signed in"))

    print(f"entitlement: {qa_cloud.read_entitlement(browser, token)}")

    _, said = qa_cloud.connect(browser, token)
    print(f"connect: {said}")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Run shared journey stories on the iPhone app.

`scripts/ios-journey.py [story...]`: each story from journeys/manifest.json
that lists the phone among its clients, against the served topology it names
for the phone (`phone_topology`, else `topology`), on the leased simulator,
driven through the app's door the way a person drives it. Screens are
compared with reviewed goldens under journeys/goldens/phone/<story>/
(UPDATE_JOURNEY_GOLDENS=1 rewrites them) and the hosts are asked what they
recorded. Results land in target/journeys/phone/<story>. With no story named,
every story written here runs.
"""

from __future__ import annotations

from pathlib import Path
import re
import signal
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
import ios_simulators  # noqa: E402
from journeys.phone import ROOT, PhoneJourney, story  # noqa: E402

SIMULATOR = "golden"


def prompts(chat: dict, text: str) -> list[dict]:
    return [item for item in chat["items"] if item["text"] == text]


def reflected_once(chat: dict, text: str) -> None:
    """The prompt the person sent is in the host's chat exactly once."""
    found = prompts(chat, text)
    if len(found) != 1:
        raise RuntimeError(f"{text!r} is in the host's chat {len(found)} times")


def negative_control(check, *args) -> str:
    """A deliberately wrong expectation must fail the same check."""
    try:
        check(*args)
    except RuntimeError as error:
        return f"a wrong expectation fails: {error}"
    raise RuntimeError(f"the check accepted a wrong outcome: {args[1:]!r}")


def pin_of(line: str) -> str | None:
    found = re.search(r"Pairing PIN: (\d{3}) (\d{3})", line)
    return found.group(1) + found.group(2) if found else None


def printed_pin(process) -> str:
    """The six digits a machine's `amux pair` printed."""
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        line = process.stdout.readline()
        if pin := pin_of(line):
            return pin
        if not line and process.poll() is not None:
            break
    raise RuntimeError("the machine printed no pairing PIN")


def trusted_as(drawn: dict, host_id: str, name: str) -> None:
    """The phone lists the machine it paired with by its own id and name."""
    row = drawn.get(f"hosts.row.{host_id}")
    if row is None or not (row.get("label") or "").startswith(f"{name}, "):
        raise RuntimeError(f"the phone does not list {name} ({host_id}) as a host: {row!r}")


def reach_host(journey: PhoneJourney) -> list[str]:
    desk = journey.host_id("desk")
    prompt = "Hello from the phone."
    reply = "The desk is reachable."
    # Unpaired: the phone's own browser finds the desk and offers to pair.
    journey.launch()
    journey.tap("tab.hosts")
    journey.wait_for(f"hosts.offer.{desk}", f"hosts.pair.{desk}")
    journey.screen("found-not-paired")

    # Six digits the desk printed, typed on the phone's keypad, reach the
    # desk's own name and key before anything is trusted.
    pairing = journey.amux("desk", "pair")
    pin = printed_pin(pairing)
    journey.tap(f"hosts.pair.{desk}")
    journey.wait_for("pin.code", "pin.key.0")
    for digit in pin:
        journey.tap(f"pin.key.{digit}")
    card = journey.wait_for("pair-confirm.trust", "pair-confirm.fingerprint")
    if card["pair-confirm.name"].get("value") != "desk":
        raise RuntimeError(f"the code reached {card['pair-confirm.name']!r}, not the desk")
    journey.screen("pair-confirm", volatile=("pair-confirm.fingerprint", "pair-confirm.expiry"))
    journey.tap("pair-confirm.trust")
    drawn = journey.wait(
        lambda drawn: f"hosts.row.{desk}" in drawn and "pair-confirm.done" in drawn, "the desk paired"
    )
    trusted_as(drawn, desk, "desk")
    control = negative_control(trusted_as, drawn, desk, "laptop")
    pairing.wait(timeout=30)
    journey.screen("paired")
    journey.tap("pair-confirm.done")
    journey.wait(lambda drawn: "pair-confirm.done" not in drawn and "tab.agents" in drawn, "the hosts tab again")

    # The desk's agent is in the fleet; the phone opens it, writes, and the
    # desk holds the prompt once and answers.
    journey.tap("tab.agents")
    agent = next(item["id"] for item in journey.ready["agents"] if item["name"] == "desk-work")
    journey.wait_for(f"home.row.{agent}")
    journey.tap(f"home.row.{agent}")
    journey.wait_for("chat.field")
    journey.type("chat.field", prompt)
    journey.wait(lambda drawn: drawn.get("chat.send", {}).get("enabled") is True, "a message ready to send")
    journey.tap("chat.send")
    heard = journey.wait_chat(
        "desk", "desk-work", lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, prompt)) == 1, "desk-heard"
    )
    reflected_once(heard, prompt)
    journey.wait(
        lambda drawn: any(reply in (element.get("label") or "") for element in drawn.values()),
        "the desk's reply on the phone",
    )

    # Leaving the chat comes back to the fleet; opened again, the chat holds
    # the prompt and the reply.
    journey.tap("chat.back")
    journey.wait(lambda drawn: f"home.row.{agent}" in drawn and "chat.field" not in drawn, "the fleet")
    journey.tap(f"home.row.{agent}")
    journey.wait(
        lambda drawn: "chat.row.turn-end" in drawn
        and any(reply in (element.get("label") or "") for element in drawn.values()),
        "the chat opened again",
    )
    journey.screen("usable-chat", volatile=("chat.row.turn-end",))
    journey.tap("chat.back")
    journey.wait(lambda drawn: f"home.row.{agent}" in drawn and "chat.field" not in drawn, "the fleet")
    return [
        "the phone's own browser found the desk in the net's scope and offered to pair",
        "the PIN the desk printed, typed on the keypad, reached the desk's name and key before trust",
        control,
        f"the phone lists desk {desk} as its host after Pair",
        f"the desk's agent opened on the phone; the desk holds {prompt!r} once and answered",
        "leaving the chat came back to the fleet, and the chat opened again held the prompt and the reply",
    ]


STORIES = {
    "reach-host": reach_host,
}


def main() -> int:
    wanted = sys.argv[1:] or list(STORIES)
    unknown = [name for name in wanted if name not in STORIES]
    if unknown:
        print(f"no phone story written for {', '.join(unknown)}; one of {', '.join(STORIES)}", file=sys.stderr)
        return 2
    udid = ios_simulators.ready(SIMULATOR)
    failed = 0
    for name in wanted:
        declared = story(name)
        topology = ROOT / declared.get("phone_topology", declared["topology"])
        began = time.monotonic()
        journey = PhoneJourney(declared, topology, udid)
        try:
            assertions = STORIES[name](journey)
            journey.finish(assertions)
            for assertion in assertions:
                print(f"- {assertion}")
            print(f"{name}: journey took {time.monotonic() - began:.1f}s", flush=True)
            # The verifier recognizes this exact final line as a pass.
            print(f"{name}: passed", flush=True)
        except BaseException as error:
            journey.fail(error)
            print(f"FAIL {name}: {error}", file=sys.stderr, flush=True)
            failed += 1
        finally:
            journey.close()
    return 1 if failed else 0


if __name__ == "__main__":
    # Let the recipe's timeout unwind the served net and the app.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    sys.exit(main())

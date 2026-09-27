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
from journeys.phone import ROOT, DoorError, PhoneJourney, story  # noqa: E402

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


PROMPT = "Check the deployment once."
REPLY = "The deploy check passed."


def labelled(drawn: dict, text: str) -> bool:
    return any(text in (element.get("label") or "") for element in drawn.values())


def choice(drawn: dict, label: str) -> str:
    """The ask choice a person reads as `label`."""
    for name, element in drawn.items():
        if name.startswith("ask.choice.") and element.get("label") == label:
            return name
    offered = [e.get("label") for n, e in drawn.items() if n.startswith("ask.choice.")]
    raise RuntimeError(f"no choice reads {label!r}; the card offers {offered!r}")


def pair_by_code(journey: PhoneJourney, host: str) -> None:
    """The phone's browser finds `host`; the PIN it printed is typed on the
    keypad and the machine it names is trusted."""
    host_id = journey.host_id(host)
    journey.tap("tab.hosts")
    journey.wait_for(f"hosts.pair.{host_id}")
    pairing = journey.amux(host, "pair")
    pin = printed_pin(pairing)
    journey.tap(f"hosts.pair.{host_id}")
    journey.wait_for("pin.key.0")
    for digit in pin:
        journey.tap(f"pin.key.{digit}")
    journey.wait_for("pair-confirm.trust")
    journey.tap("pair-confirm.trust")
    journey.wait_for("pair-confirm.done")
    journey.tap("pair-confirm.done")
    journey.wait(lambda drawn: f"hosts.row.{host_id}" in drawn and "tab.agents" in drawn, f"{host} paired")
    pairing.wait(timeout=30)


def open_agent(journey: PhoneJourney, name: str) -> str:
    agent = next(item["id"] for item in journey.ready["agents"] if item["name"] == name)
    journey.tap("tab.agents")
    journey.wait_for(f"home.row.{agent}")
    journey.tap(f"home.row.{agent}")
    journey.wait_for("chat.field")
    return agent


def send(journey: PhoneJourney, text: str) -> None:
    journey.type("chat.field", text)
    journey.wait(lambda drawn: drawn.get("chat.send", {}).get("enabled") is True, "a message ready to send")
    journey.tap("chat.send")


def conversation_decision(journey: PhoneJourney, agent: str, provider_logs: bool) -> list[str]:
    journey.launch()
    pair_by_code(journey, "desk")
    open_agent(journey, agent)
    send(journey, PROMPT)
    card = journey.wait(
        lambda drawn: "ask" in drawn and labelled(drawn, "deploy --check") and labelled(drawn, "Allow once"),
        "the permission card",
    )
    sent = journey.wait_chat("desk", agent, lambda chat: len(prompts(chat, PROMPT)) >= 1, "prompt-reflected")
    reflected_once(sent, PROMPT)
    control = negative_control(reflected_once, sent, PROMPT + " (a wrong prompt)")
    journey.screen("permission")
    journey.tap(choice(card, "Allow once"))
    journey.wait(lambda drawn: "ask" not in drawn and labelled(drawn, REPLY), "the settled turn")
    settled = journey.wait_chat(
        "desk",
        agent,
        lambda chat: chat["phase"] == "IDLE" and any(REPLY in item["text"] for item in chat["items"]),
        "turn-settled",
    )
    reflected_once(settled, PROMPT)
    assertions = [
        f"{PROMPT!r} reflected once in the desk's chat",
        control,
        "the permission card offered its outcomes and Allow once was tapped",
        f"the reply {REPLY!r} arrived and the desk says idle",
    ]
    if provider_logs:
        lines = journey.provider_input(agent, "provider-input")
        answers = [line for line in lines if "allow" in line.lower() or "accept" in line.lower()]
        if len(answers) != 1:
            raise RuntimeError(f"the provider did not receive one allow: {lines!r}")
        assertions.append("the provider received exactly one allow")
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat.field" not in drawn, "the fleet")
    journey.tap(f"home.row.{next(a['id'] for a in journey.ready['agents'] if a['name'] == agent)}")
    journey.wait(lambda drawn: "chat.row.turn-end" in drawn and labelled(drawn, REPLY), "the chat opened again")
    journey.screen("settled", volatile=("chat.row.turn-end",))
    return assertions


def never_received(chat: dict, text: str) -> None:
    if prompts(chat, text):
        raise RuntimeError(f"the desk received {text!r} while access was lost")


def switch_to(journey: PhoneJourney, account: str) -> None:
    """The account a person picks from the switcher under the title."""
    journey.tap("tab.agents")
    journey.tap("home.title")
    journey.wait_for(f"account.{account}@example.com")
    journey.tap(f"account.{account}@example.com")
    journey.wait(
        lambda drawn: (drawn.get("home.subtitle", {}).get("value") or "").startswith(f"{account} ·"),
        f"{account} on screen",
    )


def pair_through_the_relay(journey: PhoneJourney, host: str) -> None:
    """Pairs by the link `host` printed. The phone's relay link comes up a
    moment after signing in, and until it has, the link reaches nobody; the
    person scans the same code again."""
    pairing, link = journey.pairing_link(host)
    deadline = time.monotonic() + 60
    while True:
        try:
            journey.pair(link)
            break
        except DoorError:
            if time.monotonic() > deadline:
                pairing.kill()
                raise
            time.sleep(2)
    pairing.wait(timeout=30)


def keep_authority(journey: PhoneJourney) -> list[str]:
    asked = "Are you there?"
    held = "Still there?"
    desk = journey.host_id("desk")
    guarded = next(item["id"] for item in journey.ready["agents"] if item["name"] == "guarded")
    relay = journey.ready["cloud_url"]
    journey.launch()

    # Signed in to the desk's account, the phone pairs with the desk by the
    # link it printed; there is no local network between them, only the
    # relay.
    journey.app({"kind": "connect", "relay": relay, "token": "refresh-ada", "user": "ada"})
    pair_through_the_relay(journey, "desk")
    journey.tap("tab.agents")
    journey.wait_for(f"home.row.{guarded}")

    # A second account is a profile of its own, on its own empty fleet.
    journey.app({"kind": "addAccount", "user": "bob", "token": "refresh-bob"})
    journey.wait(lambda drawn: f"home.row.{guarded}" not in drawn and "home.title" in drawn, "bob's empty fleet")
    journey.tap("tab.hosts")
    drawn = journey.wait(lambda drawn: "hosts" in drawn, "bob's hosts")
    if f"hosts.row.{desk}" in drawn:
        raise RuntimeError("bob's profile knows the desk")
    journey.screen("other-account")
    switch_to(journey, "ada")
    journey.wait_for(f"home.row.{guarded}")

    # Through the relay the desk's agent answers.
    journey.tap(f"home.row.{guarded}")
    journey.wait_for("chat.field")
    send(journey, asked)
    journey.wait(lambda drawn: labelled(drawn, "Only this account reaches me."), "the first reply")
    reflected_once(journey.wait_chat("desk", "guarded", lambda chat: chat["phase"] == "IDLE", "reached"), asked)
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat.field" not in drawn, "the fleet")

    # Signed out, the phone has no way to the desk: the chat says so and a
    # message written now never reaches it.
    journey.tap("tab.you")
    journey.wait_for("you.signOut")
    journey.tap("you.signOut")
    journey.wait(
        lambda drawn: "Signed out" in (drawn.get("account.ada@example.com", {}).get("label") or ""), "signed out"
    )
    journey.tap("tab.hosts")
    journey.wait(lambda drawn: labelled(drawn, "signed out"), "the hosts tab naming the sign-out")
    journey.screen("blocked-hosts")
    journey.tap("tab.agents")
    journey.tap(f"home.row.{guarded}")
    journey.wait(lambda drawn: "chat.field" in drawn and labelled(drawn, "this phone is signed out"), "the chat naming the sign-out")
    journey.type("chat.field", held)
    drawn = journey.wait(lambda drawn: drawn.get("chat.field", {}).get("value") == held, "the draft written")
    if drawn.get("chat.send", {}).get("enabled"):
        raise RuntimeError("the phone offers to send while signed out")
    time.sleep(2)
    # The draft is kept across leaving the chat; read with the keyboard down.
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat.field" not in drawn, "the fleet")
    journey.tap(f"home.row.{guarded}")
    journey.wait(lambda drawn: drawn.get("chat.field", {}).get("value") == held, "the draft kept")
    journey.screen("blocked")
    blocked = journey.request({"Chat": {"host": "desk", "agent": "guarded"}}, "while-signed-out")
    never_received(blocked, held)
    control = negative_control(never_received, blocked, asked)

    # Signed in again: the chat is current and the held message goes once.
    journey.app({"kind": "connect", "relay": relay, "token": "refresh-ada", "user": "ada"})
    journey.wait(
        lambda drawn: not labelled(drawn, "this phone is signed out") and drawn.get("chat.send", {}).get("enabled") is True,
        "the chat current again, the draft ready to send",
    )
    journey.tap("chat.send")
    journey.wait(lambda drawn: labelled(drawn, "The relay carries us again."), "the reply after signing in")
    after = journey.wait_chat(
        "desk", "guarded", lambda chat: chat["phase"] == "IDLE" and len(prompts(chat, held)) == 1, "sent-after-sign-in"
    )
    reflected_once(after, asked)
    reflected_once(after, held)
    journey.tap("chat.back")
    journey.wait(lambda drawn: "chat.field" not in drawn, "the fleet")
    journey.tap(f"home.row.{guarded}")
    journey.wait(lambda drawn: labelled(drawn, "The relay carries us again.") and "chat.row.turn-end" in drawn, "the chat again")
    journey.screen("recovered", volatile=("chat.row.turn-end",))
    return [
        "a second account opened on its own profile, with an empty fleet that knows no desk",
        f"through the relay the desk received {asked!r} once and answered",
        "signed out, the phone named its own sign-out in the hosts tab and the chat, kept the draft and would not send it; the desk never received it",
        control,
        f"signed in again, the chat was current and {held!r} reached the desk once and was answered",
    ]


STORIES = {
    "reach-host": reach_host,
    "conversation-decision-claude-pty": lambda j: conversation_decision(j, "decision-pty", False),
    "conversation-decision-claude-sdk": lambda j: conversation_decision(j, "decision-sdk", True),
    "conversation-decision-codex": lambda j: conversation_decision(j, "decision-codex", True),
    "keep-authority": keep_authority,
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

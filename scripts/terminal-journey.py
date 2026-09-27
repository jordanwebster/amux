#!/usr/bin/env python3
"""Run a shared journey story on the real terminal client.

`scripts/terminal-journey.py <story>`: one story from journeys/manifest.json
against the served topology it names, with frames compared to reviewed
goldens (UPDATE_JOURNEY_GOLDENS=1 rewrites them) and independent
observations from the hosts. Results land in target/journeys/<story>.
"""

from __future__ import annotations

import sys
import time

from journeys.terminal import ROOT, TerminalJourney, story

PROMPT = "Check the deployment once."
REPLY = "The deploy check passed."
DRAFT = "Keep this draft through the outage."


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


def conversation_decision(journey: TerminalJourney, agent: str, provider_logs: bool) -> list[str]:
    pane = journey.launch("terminal", "desk")
    journey.open_chat(pane, agent)
    journey.type(pane, PROMPT)
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, PROMPT)
    card = journey.wait_terms(pane, "Wants to run a command", "deploy --check", "Allow once")
    sent = journey.wait_chat("desk", agent, lambda chat: len(prompts(chat, PROMPT)) >= 1, "prompt-reflected")
    reflected_once(sent, PROMPT)
    control = negative_control(reflected_once, sent, PROMPT + " (a wrong prompt)")
    journey.frame(pane, "permission")
    # The first choice is Allow once.
    journey.keys(pane, "1", "Enter")
    journey.wait(
        pane,
        lambda frame: REPLY in frame and "Wants to run a command" not in frame and "idle" in frame.splitlines()[0],
        "the settled turn",
    )
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
        "the permission card offered every outcome and Allow once was taken",
        f"the reply {REPLY!r} arrived and the desk says idle",
    ]
    if provider_logs:
        lines = journey.provider_input(agent, "provider-input")
        answers = [line for line in lines if "allow" in line.lower() or "accept" in line.lower()]
        if len(answers) != 1:
            raise RuntimeError(f"the provider did not receive one allow: {lines!r}")
        assertions.append("the provider received exactly one allow")
    journey.frame(pane, "settled")
    del card
    journey.quit_client(pane)
    assertions.append("the client exited 0")
    return assertions


def leave_and_recover(journey: TerminalJourney) -> list[str]:
    first = "The recovery state is safely stored."
    second = "A second turn arrived while you were away."
    third = "Back after the restart."
    # A real first run on the laptop makes its replica of the desk's agent.
    pane = journey.launch("first-run", "laptop")
    journey.open_chat(pane, "keeper")
    journey.wait_terms(pane, first)
    journey.wait_chat("laptop", "keeper", lambda chat: any(first in i["text"] for i in chat["items"]), "replica-made")
    journey.quit_client(pane)

    # The desk goes out of reach; a fresh client opens from the laptop's
    # store, with the composer waiting and the draft kept.
    journey.request({"Sever": {"a": "desk", "b": "laptop"}})
    pane = journey.launch("terminal", "laptop")
    journey.open_chat(pane, "keeper")
    journey.wait_terms(pane, first, "desk away · not current")
    journey.type(pane, DRAFT)
    journey.wait_terms(pane, DRAFT, "draft kept · sending waits")
    journey.frame(pane, "cached-offline")

    # Work goes on at the desk while the laptop is cut off; when the link
    # returns the delta appends once and the composer is live again.
    journey.request({"Send": {"agent": "keeper", "text": "Carry on."}})
    journey.wait_chat("desk", "keeper", lambda chat: any(second in i["text"] for i in chat["items"]), "desk-moved-on")
    journey.request({"Restore": {"a": "desk", "b": "laptop"}})
    journey.wait(
        pane,
        lambda frame: second in frame and "not current" not in frame and DRAFT in frame,
        "the reconciled chat with the draft kept",
    )
    desk = journey.chat("desk", "keeper", "desk-after-restore")
    laptop = journey.wait_chat(
        "laptop",
        "keeper",
        lambda chat: [i["key"] for i in chat["items"]] == [i["key"] for i in desk["items"]],
        "laptop-matches-desk",
    )
    if sum(second in item["text"] for item in laptop["items"]) != 1:
        raise RuntimeError(f"the delta arrived other than once: {laptop!r}")
    journey.frame(pane, "reconciled")

    # The laptop's own daemon restarts under the open chat: the client
    # reconnects by itself, keeps the chat and the draft, and sends.
    journey.request({"RestartDaemon": {"host": "laptop"}})
    journey.wait(
        pane,
        lambda frame: DRAFT in frame and "enter send" in frame and second in frame,
        "the chat live again after the restart",
    )
    journey.keys(pane, "Enter")
    journey.wait_terms(pane, third)
    after = journey.wait_chat("desk", "keeper", lambda chat: any(third in i["text"] for i in chat["items"]), "sent-after-restart")
    reflected_once(after, DRAFT)
    journey.frame(pane, "after-restart")
    # The desk rewinds: the laptop's replica is Reset, and the rows stay on
    # screen until the rebuilt transcript swaps in.
    journey.request({"Checkpoint": {"host": "desk"}})
    journey.request({"Rewind": {"host": "desk", "cuts": []}})
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        screen = journey.capture(pane)
        if first not in screen or third not in screen:
            raise RuntimeError(f"rows left the screen during the Reset:\n{screen}")
        time.sleep(0.05)
    desk = journey.chat("desk", "keeper", "desk-after-rewind")
    journey.wait_chat(
        "laptop",
        "keeper",
        lambda chat: [i["key"] for i in chat["items"]] == [i["key"] for i in desk["items"]],
        "laptop-after-reset",
    )
    # A restored drive has no running processes: the agent reads exited.
    journey.wait(pane, lambda frame: "not current" not in frame and third in frame and "exited" in frame, "the chat after the Reset")
    journey.frame(pane, "after-reset")

    journey.quit_client(pane)
    return [
        "a fresh client painted the cached chat from the laptop's store with the desk away and the composer waiting",
        "the draft typed while detached was kept through the outage and the daemon restart",
        "the desk's second turn appended once when the link returned; the laptop holds exactly the desk's items",
        "after the laptop's daemon restarted the client reconnected on its own and the draft was sent and answered",
        "rows stayed on screen through the desk's rewind and Reset until the rebuilt transcript swapped in",
        "the client exited 0",
    ]


STORIES = {
    "conversation-decision-claude-pty": lambda j: conversation_decision(j, "decision-pty", False),
    "conversation-decision-claude-sdk": lambda j: conversation_decision(j, "decision-sdk", True),
    "conversation-decision-codex": lambda j: conversation_decision(j, "decision-codex", True),
    "leave-and-recover": leave_and_recover,
}


def main() -> int:
    if len(sys.argv) != 2 or sys.argv[1] not in STORIES:
        print(f"usage: terminal-journey.py {{{','.join(STORIES)}}}", file=sys.stderr)
        return 2
    name = sys.argv[1]
    declared = story(name)
    journey = TerminalJourney(declared, ROOT / declared["topology"])
    try:
        assertions = STORIES[name](journey)
        journey.finish(assertions)
        print(f"PASS {name}")
        for assertion in assertions:
            print(f"- {assertion}")
        return 0
    except BaseException as error:
        journey.fail(error)
        print(f"FAIL {name}: {error}", file=sys.stderr)
        return 1
    finally:
        journey.close()


if __name__ == "__main__":
    sys.exit(main())

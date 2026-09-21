#!/usr/bin/env python3
"""Run the terminal scenarios declared in the shared journey manifest."""

from __future__ import annotations

from pathlib import Path
import socket
import subprocess
import sys
import threading

from journeys.terminal import AMUX, ROOT, TerminalJourney, story

PROMPT = "Check the deployment once."
RECOVERY_PROMPT = "Remember this recovery turn."
RECOVERY_REPLY = "The recovery state is safely stored."


def prompts(rows: list[dict]) -> list[dict]:
    return [row for row in rows if row.get("intent") == "prompt"]


def conversation_decision(journey: TerminalJourney, wrong: bool) -> list[str]:
    pane = journey.launch("terminal")
    journey.open_chat(pane, "decision-agent")
    journey.type(pane, PROMPT)
    journey.keys(pane, "Enter")
    expected = "deliberately wrong prompt" if wrong else PROMPT
    sent = journey.wait_observation(
        "decision-agent",
        lambda rows: len(prompts(rows)) == 1 and prompts(rows)[0].get("text") == expected,
        "exactly-one-reflected-prompt",
        timeout=2 if wrong else 60,
    )
    if len(prompts(sent)) != 1:
        raise RuntimeError(f"prompt was not reflected exactly once: {sent!r}")
    journey.wait_terms(pane, PROMPT)
    journey.request(
        {
            "AgentRaiseAsk": {
                "agent": "decision-agent",
                "ask": {
                    "Permission": {
                        "tool": "Bash",
                        "invocation": {"command": "deploy --check"},
                        "scoped_directories": ["/workspace"],
                    }
                },
            }
        }
    )
    journey.wait_terms(pane, "Allow once", "deploy --check")
    journey.frame(pane, "permission")
    journey.keys(pane, "1", "Enter")
    answered = journey.wait_observation(
        "decision-agent",
        lambda rows: len(rows) == 2
        and rows[1].get("intent") == "answer"
        and rows[1].get("answer", {}).get("permission") == "allow_once",
        "permission-allowed-once",
    )
    if len([row for row in answered if row.get("intent") == "answer"]) != 1:
        raise RuntimeError(f"permission was not answered exactly once: {answered!r}")
    journey.request({"AgentEndTurn": {"agent": "decision-agent"}})
    journey.wait_terms(pane, "Type a message")
    journey.frame(pane, "finished")
    journey.stop_client(pane)
    return [
        "the door observed exactly one reflected prompt",
        "one permission ask was answered allow-once exactly once",
        "the terminal showed the finished turn and exited zero",
    ]


def dump(config: Path, agent_id: str) -> str:
    result = subprocess.run(
        [str(AMUX), "--config", str(config), "store", "dump", agent_id],
        cwd=ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=30,
    )
    return result.stdout


def leave_and_recover(journey: TerminalJourney, wrong: bool) -> list[str]:
    pane = journey.launch("first-run")
    journey.open_chat(pane, "recovery-agent")
    journey.type(pane, RECOVERY_PROMPT)
    journey.keys(pane, "Enter")
    journey.wait_observation(
        "recovery-agent",
        lambda rows: len(prompts(rows)) == 1 and prompts(rows)[0].get("text") == RECOVERY_PROMPT,
        "first-run-prompt",
    )
    journey.request({"AgentPlay": {"agent": "recovery-agent", "steps": [
        {"Markdown": {"text": RECOVERY_REPLY}}, "EndTurn"
    ]}})
    journey.wait_terms(pane, RECOVERY_REPLY, "Type a message")
    online = journey.frame(pane, "online")
    if wrong and "a deliberately absent row" not in online.text:
        raise RuntimeError("deliberately wrong expected observation was absent")
    inventory = journey.request({"Inventory": {"daemon": "terminal-host"}})
    agent_id = next(
        item["id"]
        for item in inventory["agents"]
        if item["name"] == "recovery-agent"
    )
    before = dump(journey.config, agent_id)
    journey.kill_client(pane)
    profile_socket = Path(next(
        line.split(": ", 1)[1]
        for line in journey.config.read_text().splitlines()
        if line.startswith("socket_path: ")
    ))
    live_socket = profile_socket.with_name(profile_socket.name + ".live")
    profile_socket.rename(live_socket)
    blackhole = socket.socket(socket.AF_UNIX)
    accepted: list[socket.socket] = []
    try:
        blackhole.bind(str(profile_socket))
        blackhole.listen()

        def accept_without_answering() -> None:
            try:
                connection, _ = blackhole.accept()
                accepted.append(connection)
            except OSError:
                pass

        threading.Thread(target=accept_without_answering, daemon=True).start()
        pane = journey.launch("offline")
        journey.wait_terms(pane, "recovery-agent", "remembered", timeout=90)
        journey.open_chat(pane, "recovery-agent")
        journey.wait_terms(pane, RECOVERY_REPLY, "chat input unavailable", timeout=90)
        journey.frame(pane, "offline")
    finally:
        blackhole.close()
        for connection in accepted:
            connection.close()
        profile_socket.unlink(missing_ok=True)
        live_socket.rename(profile_socket)
    journey.wait_terms(pane, RECOVERY_REPLY, "Type a message", timeout=90)
    journey.frame(pane, "reconciled")
    after = dump(journey.config, agent_id)
    for value in (RECOVERY_PROMPT, RECOVERY_REPLY):
        if before.count(value) != 1 or after.count(value) != 1:
            raise RuntimeError(
                f"cache reconciliation duplicated or lost {value!r}: "
                f"before={before.count(value)} after={after.count(value)}"
            )
    journey.stop_client(pane)
    return [
        "a real first run produced the cached prompt and reply",
        "the relaunched offline client showed remembered fleet and unavailable composer",
        "the returning daemon reconciled without a duplicate or missing prompt or reply",
        "the terminal exited zero after recovery",
    ]


def main() -> int:
    args = sys.argv[1:]
    wrong = False
    if args and args[0] == "--expect-wrong":
        wrong = True
        args.pop(0)
    if len(args) != 1:
        raise SystemExit("usage: terminal-journey.py [--expect-wrong] NAME")
    declared = story(args[0])
    journey = TerminalJourney(declared)
    try:
        if declared["id"] == "conversation-decision-claude-pty":
            assertions = conversation_decision(journey, wrong)
        elif declared["id"] == "leave-and-recover":
            assertions = leave_and_recover(journey, wrong)
        else:
            raise RuntimeError(f"terminal story has no scenario: {declared['id']}")
        journey.finish(assertions)
        print(f"{declared['id']}: passed")
        return 0
    except BaseException as error:
        journey.fail(error)
        raise
    finally:
        journey.close()


if __name__ == "__main__":
    raise SystemExit(main())

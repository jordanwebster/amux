#!/usr/bin/env python3
"""Run the terminal scenarios declared in the shared journey manifest."""

from __future__ import annotations

from pathlib import Path
import socket
import subprocess
import sys
import threading
import time

from journeys.terminal import AMUX, ROOT, TerminalJourney, story

PROMPT = "Check the deployment once."
RECOVERY_PROMPT = "Remember this recovery turn."
RECOVERY_REPLY = "The recovery state is safely stored."
REACH_PROMPT = "Open the remote work."
REACH_REPLY = "The remote work is open."
PAIRED_PROMPT = "Open the work on the machine I just trusted."
PAIRED_REPLY = "The newly paired machine is reachable."
SHARED_PROMPT = "Show this to both terminals."
SHARED_REPLY = "Both terminals received this reply."


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
    journey.wait_terms(
        pane,
        "Allow once",
        "▸ Bash deploy --check",
        "└ running",
    )
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
    journey.wait_terms(
        pane,
        "default · idle",
        "✔ Bash deploy --check",
        "Permission granted.",
        "─ turn ·",
        "enter send",
    )
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


def run_amux(config: Path, *args: str) -> str:
    result = subprocess.run(
        [str(AMUX), "--config", str(config), *args],
        cwd=ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=60,
    )
    return result.stdout + result.stderr


def pair_with_pin(config: Path, target: str, pin: str) -> str:
    result = subprocess.run(
        [str(AMUX), "--config", str(config), "pair", target],
        cwd=ROOT,
        input=f"{pin}\n",
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=120,
    )
    if result.returncode != 0:
        raise RuntimeError(f"pairing with {target} failed ({result.returncode}): {result.stdout}")
    return result.stdout


def peer_row(listing: str, host: str) -> str:
    rows = [line for line in listing.splitlines() if line.split(" ", 1)[0] == host]
    if len(rows) != 1:
        raise RuntimeError(f"peer list has {len(rows)} rows for {host}:\n{listing}")
    return rows[0]


def find_and_pair(journey: TerminalJourney, wrong: bool) -> list[str]:
    pane = journey.launch("onramp")
    journey.wait_terms(pane, "┌ amux")

    # The machine is on the network but nothing trusts it: the listing must
    # say so, and must say what would change it, because that line is the
    # whole of the terminal's onramp instructions.
    found = run_amux(journey.config, "peer", "list")
    row = peer_row(found, "workstation")
    for term in ("found · on this network", "pair with: amux pair workstation"):
        if term not in row:
            raise RuntimeError(f"workstation was not offered as a candidate: {row!r}")
    journey.observations["candidate-listing"] = found

    # The overlay is the terminal's own account of what it can reach. Before
    # pairing it knows only this machine: a found candidate reaches the
    # listing above but not the streamed host model behind this screen.
    journey.keys(pane, "h")
    overlay = journey.wait_terms(pane, "hosts", "terminal-host")
    if "workstation" in overlay:
        raise RuntimeError(f"an unpaired machine appeared as a host: {overlay!r}")
    journey.keys(pane, "Escape")
    journey.wait(pane, lambda frame: "any key to close" not in frame, "fleet after the overlay")
    journey.frame(pane, "found")

    issued = journey.request(
        {"StartPinPairing": {"daemon": "workstation", "ttl_secs": 300}},
        "pairing-code-issued",
    )
    pin = issued.get("pin")
    if not isinstance(pin, str) or len(pin) != 6:
        raise RuntimeError(f"workstation issued no six-digit pairing code: {issued!r}")
    paired = pair_with_pin(journey.config, "workstation", pin)
    journey.observations["pairing-output"] = paired
    if "Paired with workstation" not in paired or "Fingerprint: " not in paired:
        raise RuntimeError(f"pairing did not report an identity and a peer: {paired!r}")

    trusted = run_amux(journey.config, "peer", "list")
    row = peer_row(trusted, "workstation")
    if not row.endswith("yes") or " direct " not in row:
        raise RuntimeError(f"workstation is not a trusted direct peer: {row!r}")
    journey.observations["trusted-listing"] = trusted

    # The same terminal, never restarted, must revise what it says about the
    # machine: trust that only a relaunch can see is not trust the person got.
    journey.keys(pane, "h")
    journey.wait_terms(pane, "hosts", "workstation", "direct", timeout=90)
    journey.keys(pane, "Escape")
    journey.wait(pane, lambda frame: "any key to close" not in frame, "fleet after the second overlay")

    # A newly trusted machine's work has to be dialed and listed before it can
    # be opened, which is slower than anything else in this story.
    journey.wait_terms(pane, "workstation-agent", timeout=90)
    journey.open_chat(pane, "workstation-agent")
    journey.type(pane, PAIRED_PROMPT)
    journey.keys(pane, "Enter")
    expected = "deliberately wrong paired prompt" if wrong else PAIRED_PROMPT
    observed = journey.wait_observation(
        "workstation-agent",
        lambda rows: len(prompts(rows)) == 1 and prompts(rows)[0].get("text") == expected,
        "paired-host-received-prompt",
        timeout=2 if wrong else 60,
    )
    if wrong and observed:
        raise RuntimeError("deliberately wrong find-and-pair expectation")
    journey.request(
        {
            "AgentPlay": {
                "agent": "workstation-agent",
                "steps": [{"Markdown": {"text": PAIRED_REPLY}}, "EndTurn"],
            }
        }
    )
    journey.wait_terms(
        pane,
        PAIRED_PROMPT,
        PAIRED_REPLY,
        "default · idle",
        "─ turn ·",
        "enter send",
    )
    journey.frame(pane, "paired")
    journey.stop_client(pane)
    return [
        "the unpaired machine was listed as found, with what would pair it",
        "six digits it issued made it a trusted direct peer with its own identity",
        "the running terminal revised its hosts overlay without a relaunch",
        "work on the newly paired machine opened and the machine recorded the prompt",
    ]


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
    journey.wait_terms(
        pane,
        RECOVERY_REPLY,
        "default · idle",
        "─ turn ·",
        "enter send",
    )
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
    journey.wait_terms(
        pane,
        RECOVERY_REPLY,
        "chat · idle",
        "─ turn ·",
        "enter send",
        timeout=90,
    )
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


def reach_host(journey: TerminalJourney, wrong: bool) -> list[str]:
    pane = journey.launch("reach")
    journey.open_chat(pane, "reach-agent")
    journey.type(pane, REACH_PROMPT)
    journey.keys(pane, "Enter")
    observed = journey.wait_observation(
        "reach-agent",
        lambda rows: len(prompts(rows)) == 1
        and prompts(rows)[0].get("text") == REACH_PROMPT,
        "remote-host-received-prompt",
    )
    if wrong and observed:
        raise RuntimeError("deliberately wrong reach-host expectation")
    journey.request(
        {
            "AgentPlay": {
                "agent": "reach-agent",
                "steps": [{"Markdown": {"text": REACH_REPLY}}, "EndTurn"],
            }
        }
    )
    journey.wait_terms(
        pane,
        REACH_PROMPT,
        REACH_REPLY,
        "default · idle",
        "─ turn ·",
        "enter send",
    )
    journey.frame(pane, "remote-work")
    inventory = journey.request(
        {"Inventory": {"daemon": "remote-host"}}, "remote-host-inventory"
    )
    if not any(item.get("name") == "reach-agent" for item in inventory["agents"]):
        raise RuntimeError(f"remote host omitted reach-agent: {inventory!r}")
    journey.stop_client(pane)
    return [
        "the real terminal opened work owned by the remote host",
        "the remote provider observed exactly one prompt",
        "the terminal rendered the remote reply and exited zero",
    ]


def authority_boundaries(journey: TerminalJourney, wrong: bool) -> list[str]:
    pane = journey.launch("authority")
    journey.open_chat(pane, "authority-agent")
    before = journey.observe("authority-agent", "before-entitlement-loss")
    journey.request(
        {"UdpBlocked": {"daemon": "terminal-host", "blocked": True}}
    )
    journey.request({"UdpBlocked": {"daemon": "remote-host", "blocked": True}})
    journey.request(
        {"SeverDirect": {"a": "terminal-host", "b": "remote-host"}}
    )
    journey.request({"Tier": {"user": "personal", "tier": "free"}})
    journey.request({"RefreshEntitlement": {"name": "terminal-host"}})
    journey.request({"RefreshEntitlement": {"name": "remote-host"}})
    # Losing the link is a transition: while the client still holds the
    # agent's chat layer the composer says its session state is unknown, and
    # once the unreachable agent has been let go it says the chat is
    # unavailable. Only the second is a resting state, so the screen is
    # photographed there; catching the transition is what made this frame
    # depend on how fast the runner was.
    unavailable = journey.wait(
        pane,
        lambda frame: "authority-agent" in frame
        and "chat input unavailable for this agent" in frame,
        "settled unreachable composer",
        timeout=90,
    )
    if wrong and "deliberately absent authority state" not in unavailable:
        raise RuntimeError("deliberately wrong authority expectation")
    journey.frame(pane, "refused")
    journey.type(pane, "this write must be refused")
    journey.keys(pane, "Enter")
    time.sleep(1.0)
    refused = journey.observe("authority-agent", "after-refused-write")
    if refused != before:
        raise RuntimeError(f"authority loss delivered a refused write: {refused!r}")
    journey.keys(pane, "C-u")

    journey.request({"Tier": {"user": "personal", "tier": "pro"}})
    journey.request({"RefreshEntitlement": {"name": "terminal-host"}})
    journey.request({"RefreshEntitlement": {"name": "remote-host"}})
    journey.request(
        {"Connections": {"daemon": "terminal-host"}},
        "connections-after-entitlement-restored",
    )
    journey.request(
        {"DebugDump": {"daemon": "terminal-host", "verbose": False}},
        "terminal-host-after-entitlement-restored",
    )
    journey.keys(pane, "C-a", "s")
    journey.filter_agent(pane, "authority-agent")
    journey.wait(
        pane,
        lambda frame: "┌ amux" in frame
        and "authority-agent" in frame
        and "1/3" in frame
        and "Type a message" not in frame,
        "fleet after authority restoration",
        timeout=90,
    )
    journey.frame(pane, "restored")
    journey.keys(pane, "q")
    journey.wait_terms(pane, "AMUX_EXIT_0", timeout=30)
    return [
        "lost entitlement kept the remote work visible but disabled its composer",
        "the host observed no prompt from the refused write",
        "restored entitlement re-established the relay route and exposed the remote work",
    ]


def agent_lifecycle(journey: TerminalJourney, wrong: bool) -> list[str]:
    owner = journey.launch_agent("lifecycle-owner", "lifecycle-agent")
    journey.type(owner, "ready before suspend")
    journey.keys(owner, "Enter")
    journey.wait_terms(owner, "echo: ready before suspend")
    pane = journey.launch("lifecycle")
    journey.wait_terms(
        pane,
        "lifecycle-agent",
        "4 agents",
        "connected · 2 hosts",
    )
    journey.filter_agent(pane, "lifecycle-agent")
    journey.frame(pane, "running")
    suspended = run_amux(journey.config, "server", "suspend")
    if "Suspended 1 agent(s)." not in suspended:
        raise RuntimeError(f"unexpected suspend output: {suspended!r}")
    journey.wait_terms(owner, "[server suspending]", "AMUX_EXIT_1")
    journey.kill_client(pane)
    resumed = run_amux(journey.config, "server", "resume")
    if "Resumed 1 agent(s)." not in resumed:
        raise RuntimeError(f"unexpected resume output: {resumed!r}")
    pane = journey.launch("lifecycle-resumed")
    journey.wait_terms(
        pane,
        "lifecycle-agent",
        "4 agents",
        "connected · 2 hosts",
        timeout=90,
    )
    journey.filter_agent(pane, "lifecycle-agent")
    journey.frame(pane, "resumed")
    journey.clear_filter(pane)
    removed = run_amux(journey.config, "rm", "lifecycle-agent", "--force")
    if wrong and "deliberately not removed" not in removed:
        raise RuntimeError("deliberately wrong lifecycle expectation")
    journey.wait(
        pane,
        lambda frame: "lifecycle-agent" not in frame
        and "3 agents" in frame
        and "connected · 2 hosts" in frame
        and "enter chat" in frame
        and "report written" not in frame,
        "agent removed from fleet",
        timeout=90,
    )
    journey.filter_agent(pane, "shared-agent")
    journey.frame(pane, "removed")
    journey.keys(pane, "q")
    journey.wait_terms(pane, "AMUX_EXIT_0", timeout=30)
    return [
        "a real test-agent was created and visible in the terminal UI",
        "installation suspend notified the attached process and resume restored it",
        "deleting the resumed agent removed it from the terminal fleet",
    ]


def second_attach(journey: TerminalJourney, wrong: bool) -> list[str]:
    def wait_for_replay(pane: str) -> str:
        return journey.wait(
            pane,
            lambda frame: SHARED_PROMPT in frame
            and SHARED_REPLY in frame
            and any(header in frame for header in ("chat · idle", "default · idle"))
            and "─ turn ·" in frame
            and "enter send" in frame,
            "settled shared replay",
        )

    first = journey.launch("first")
    second = journey.launch("second")
    journey.open_chat(first, "shared-agent")
    journey.open_chat(second, "shared-agent")
    journey.type(first, SHARED_PROMPT)
    journey.keys(first, "Enter")
    journey.wait_observation(
        "shared-agent",
        lambda rows: len(prompts(rows)) == 1
        and prompts(rows)[0].get("text") == SHARED_PROMPT,
        "one-shared-prompt",
    )
    journey.request(
        {
            "AgentPlay": {
                "agent": "shared-agent",
                "steps": [{"Markdown": {"text": SHARED_REPLY}}, "EndTurn"],
            }
        }
    )
    wait_for_replay(first)
    second_frame = wait_for_replay(second)
    if wrong and "deliberately missing replay" not in second_frame:
        raise RuntimeError("deliberately wrong second-attach expectation")
    journey.frame(second, "attached-replay", normalize_session_idle=True)

    journey.stop_client(first)
    journey.tmux("resize-window", "-t", second, "-x", "96", "-y", "32")
    wait_for_replay(second)
    journey.frame(second, "resized", normalize_session_idle=True)
    journey.keys(second, "C-a", "s")
    journey.wait(
        second,
        lambda frame: "┌ amux" in frame and "Type a message" not in frame,
        "fleet after detach",
    )
    journey.open_chat(second, "shared-agent")
    wait_for_replay(second)
    journey.frame(second, "reopened", normalize_session_idle=True)
    journey.stop_client(second)
    return [
        "both real terminals received one prompt and its reply",
        "the first terminal detached and exited without ending the session",
        "the resized second terminal left and reopened the replayed conversation",
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
        if declared["id"] == "find-and-pair":
            assertions = find_and_pair(journey, wrong)
        elif declared["id"] == "conversation-decision-claude-pty":
            assertions = conversation_decision(journey, wrong)
        elif declared["id"] == "leave-and-recover":
            assertions = leave_and_recover(journey, wrong)
        elif declared["id"] == "reach-host":
            assertions = reach_host(journey, wrong)
        elif declared["id"] == "authority-boundaries":
            assertions = authority_boundaries(journey, wrong)
        elif declared["id"] == "agent-lifecycle":
            assertions = agent_lifecycle(journey, wrong)
        elif declared["id"] == "second-attach":
            assertions = second_attach(journey, wrong)
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

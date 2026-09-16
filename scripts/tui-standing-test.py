#!/usr/bin/env python3
"""Prove two real TUIs consume daemon summaries without opening every chat."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import shlex
import socket
import subprocess
import sys
import tempfile
import time
import traceback

ROOT = Path(__file__).resolve().parent.parent
TOPOLOGY = ROOT / "e2e-tests/topologies/tui-standing.json"
AMUX = ROOT / "target/debug/amux"
TESTNET = ROOT / "target/debug/testnet"


def ask(address: str, request: object) -> dict:
    host, port = address.rsplit(":", 1)
    with socket.create_connection((host, int(port)), timeout=30) as connection:
        connection.sendall((json.dumps(request) + "\n").encode())
        with connection.makefile("rb") as stream:
            encoded = stream.readline()
            try:
                reply = json.loads(encoded)
            except json.JSONDecodeError as error:
                raise RuntimeError(f"invalid control reply {encoded!r}") from error
    if "Ack" not in reply:
        raise RuntimeError(f"testnet refused {request}: {reply}")
    return reply["Ack"]


def tmux(server: str, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["tmux", "-L", server, *args],
        check=check,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def capture(server: str, target: str) -> str:
    return tmux(server, "capture-pane", "-p", "-S", "-40", "-t", target).stdout


def frame_with(server: str, target: str, *terms: str) -> str | None:
    frame = capture(server, target)
    return frame if all(term in frame for term in terms) else None


def wait_for(description: str, probe, timeout: float = 30.0):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        last = probe()
        if last:
            return last
        time.sleep(0.1)
    raise RuntimeError(f"timed out waiting for {description}; last observation: {last!r}")


def read_readiness(process: subprocess.Popen[str], timeout: float = 60.0) -> dict:
    assert process.stdout is not None
    deadline = time.monotonic() + timeout
    observed: list[str] = []
    while time.monotonic() < deadline:
        line = process.stdout.readline()
        if not line:
            if process.poll() is not None:
                raise RuntimeError(
                    f"testnet exited before readiness ({process.returncode}): {observed!r}"
                )
            continue
        if not line.strip():
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            observed.append(line.rstrip())
            continue
        if isinstance(value, dict) and "control" in value and "daemons" in value:
            return value
        observed.append(line.rstrip())
    raise RuntimeError(f"timed out waiting for testnet readiness: {observed!r}")


def recent_subscriptions(value: object, agent_name: str) -> list[dict]:
    def nested(candidate: object) -> list[dict] | None:
        if isinstance(candidate, dict):
            records = candidate.get("recent_subscriptions")
            if isinstance(records, list):
                return records
            for child in candidate.values():
                found = nested(child)
                if found is not None:
                    return found
        elif isinstance(candidate, list):
            for child in candidate:
                found = nested(child)
                if found is not None:
                    return found
        return None

    if isinstance(value, dict):
        agent = value.get("agent")
        if value.get("name") == agent_name or (
            isinstance(agent, dict) and agent.get("name") == agent_name
        ):
            return nested(value) or []
        for child in value.values():
            found = recent_subscriptions(child, agent_name)
            if found:
                return found
    elif isinstance(value, list):
        for child in value:
            found = recent_subscriptions(child, agent_name)
            if found:
                return found
    return []


def main() -> int:
    if shutil.which("tmux") is None:
        raise RuntimeError("tmux is required for the standing test")
    evidence = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else None
    owned_evidence = tempfile.TemporaryDirectory(prefix="amux-tui-standing-evidence-")
    output = evidence or Path(owned_evidence.name)
    output.mkdir(parents=True, exist_ok=True)

    with tempfile.TemporaryDirectory(prefix="amux-ts-", dir="/tmp") as temporary:
        env = os.environ.copy()
        env.update({key: temporary for key in ("TMPDIR", "TMP", "TEMP")})
        testnet = subprocess.Popen(
            [str(TESTNET), "serve", "--topology", str(TOPOLOGY)],
            cwd=ROOT,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        server = f"amux-standing-{os.getpid()}"
        transcript: list[str] = []
        try:
            ready = read_readiness(testnet)
            control = ready["control"]
            daemon = next(item for item in ready["daemons"] if item["name"] == "host")
            ids = {item["name"]: item["agent_id"] for item in ready["agents"]}
            command = (
                f"env TERM=xterm-256color AMUX_TUI_DIRECT_PROFILE=1 "
                f"AMUX_TUI_EAGER_EXCLUDE={ids['fleet-only']} "
                f"{shlex.quote(str(AMUX))} --config "
                f"{shlex.quote(daemon['profile_config'])} ui"
            )
            common = "sh -c " + shlex.quote(
                command + "; status=$?; echo AMUX_EXIT_$status; sleep 120"
            )
            tmux(server, "new-session", "-d", "-x", "120", "-y", "30", "-s", "standing", "-n", "one", common)
            tmux(server, "new-window", "-d", "-t", "standing", "-n", "two", common)
            panes = ["standing:one", "standing:two"]
            for pane in panes:
                wait_for(
                    f"three fleet rows in {pane}",
                    lambda pane=pane: frame_with(server, pane, *ids.keys()),
                    60,
                )

            for pane, name in zip(panes, ["first-chat", "second-chat"]):
                transcript.append(f"{pane}: filter /{name}, open chat, return with C-a s")
                tmux(server, "send-keys", "-t", pane, "/", name)
                wait_for(
                    f"{name} filtered in {pane}",
                    lambda pane=pane, name=name: frame_with(server, pane, name, "1/3"),
                )
                tmux(server, "send-keys", "-t", pane, "Escape")
                wait_for(
                    f"navigation mode in {pane}",
                    lambda pane=pane: frame_with(server, pane, "enter raw attach", "o chat"),
                )
                tmux(server, "send-keys", "-t", pane, "o")
                wait_for(
                    f"{name} chat in {pane}",
                    lambda pane=pane, name=name: frame_with(
                        server, pane, name, "chat ·"
                    ),
                )
                tmux(server, "send-keys", "-t", pane, "C-a", "s")
                wait_for(
                    f"filtered fleet restored in {pane}",
                    lambda pane=pane, name=name: frame_with(
                        server, pane, name, "1/3", "enter raw attach"
                    ),
                )
                tmux(server, "send-keys", "-t", pane, "/")
                wait_for(
                    f"filter focused in {pane}",
                    lambda pane=pane: frame_with(server, pane, "esc nav-mode"),
                )
                tmux(server, "send-keys", "-t", pane, "C-c")
                wait_for(
                    f"filter cleared in {pane}",
                    lambda pane=pane: frame_with(server, pane, "fleet-only", "3/3"),
                )
                tmux(server, "send-keys", "-t", pane, "Escape")
                wait_for(
                    f"fleet restored in {pane}",
                    lambda pane=pane: frame_with(server, pane, "fleet-only", "agents"),
                )

            before = [capture(server, pane) for pane in panes]
            diagnostics = ask(control, {"DebugDump": {"daemon": "host", "verbose": True}})["diagnostics"]
            (output / "diagnostics.json").write_text(json.dumps(diagnostics, indent=2) + "\n")
            subscriptions = recent_subscriptions(diagnostics, "fleet-only")
            if not subscriptions:
                raise RuntimeError("fleet-only diagnostics omitted the summarizer subscription")
            non_summarizer = [record for record in subscriptions if record.get("query") != "tail 0"]
            if non_summarizer:
                raise RuntimeError(f"fleet-only opened a client subscription: {non_summarizer}")

            transcript.append("control: fleet-only runs and ends one scripted turn")
            ask(
                control,
                {
                    "AgentPlay": {
                        "agent": "fleet-only",
                        "steps": [
                            {"Prompt": {"text": "Complete the unattended task."}},
                            "EndTurn",
                        ],
                    }
                },
            )
            after = []
            for pane in panes:
                after.append(
                    wait_for(
                        f"fleet-only finished standing in {pane}",
                        lambda pane=pane: frame_with(
                            server, pane, "fleet-only", "finished"
                        ),
                        30,
                    )
                )

            frames = []
            for number, (pane, frame) in enumerate(zip(panes, before), 1):
                frames.append(f"FRAME {number} {pane} before finish\n{frame}")
            for number, (pane, frame) in enumerate(zip(panes, after), 3):
                frames.append(f"FRAME {number} {pane} after finish\n{frame}")
            (output / "frames.txt").write_text("\n\n".join(frames))
            (output / "subscriptions.txt").write_text(json.dumps(subscriptions, indent=2) + "\n")
            (output / "typescript").write_text("\n".join(transcript) + "\n")
            (output / "result.txt").write_text("PASS\n")
            (output / "failure-frames.txt").unlink(missing_ok=True)
            print(f"standing PASS; evidence: {output}")
            return 0
        except Exception as error:
            (output / "result.txt").write_text(f"FAIL: {error}\n{traceback.format_exc()}")
            if "panes" in locals():
                failed = []
                for pane in panes:
                    result = tmux(server, "capture-pane", "-p", "-S", "-80", "-t", pane, check=False)
                    failed.append(f"{pane}\nstdout:\n{result.stdout}\nstderr:\n{result.stderr}")
                (output / "failure-frames.txt").write_text("\n\n".join(failed))
            print(f"standing FAIL: {error}", file=sys.stderr)
            return 1
        finally:
            tmux(server, "kill-server", check=False)
            if testnet.poll() is None:
                try:
                    if "control" in locals():
                        ask(control, "Shutdown")
                except Exception:
                    testnet.terminate()
                try:
                    testnet.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    testnet.kill()
                    testnet.wait(timeout=5)


if __name__ == "__main__":
    raise SystemExit(main())

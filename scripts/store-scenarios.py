#!/usr/bin/env python3
"""Replay the shared-store desktop journeys through tmux and one daemon each."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import shutil
import shlex
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import traceback
import uuid

ROOT = Path(__file__).resolve().parent.parent
AMUX = ROOT / "target/debug/amux"
TESTNET = ROOT / "target/debug/testnet"
WRITING = ROOT / "e2e-tests/scripts/writing.json"
SDK_SCRIPT = ROOT / "e2e-tests/scripts/sdk-sessions.json"
SCENARIOS = ("warm-start", "chat", "two-terminal", "gap", "sdk-resume")


def ask(address: str, request: object, timeout: float = 60.0) -> dict:
    host, port = address.rsplit(":", 1)
    with socket.create_connection((host, int(port)), timeout=timeout) as connection:
        connection.settimeout(timeout)
        connection.sendall((json.dumps(request, separators=(",", ":")) + "\n").encode())
        with connection.makefile("rb") as stream:
            encoded = stream.readline()
    try:
        reply = json.loads(encoded)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"invalid control reply {encoded!r}") from error
    if "Ack" not in reply:
        raise RuntimeError(f"testnet refused {request}: {reply}")
    return reply["Ack"]


def read_readiness(process: subprocess.Popen[str], timeout: float = 60.0) -> dict:
    assert process.stdout is not None
    deadline = time.monotonic() + timeout
    seen: list[str] = []
    while time.monotonic() < deadline:
        line = process.stdout.readline()
        if not line:
            if process.poll() is not None:
                raise RuntimeError(
                    f"testnet exited before readiness ({process.returncode}): {seen!r}"
                )
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            seen.append(line.rstrip())
            continue
        if isinstance(value, dict) and "control" in value and "daemons" in value:
            return value
        seen.append(line.rstrip())
    raise RuntimeError(f"timed out waiting for testnet readiness: {seen!r}")


def row(number: int, text: str, session: str) -> dict:
    row_id = str(uuid.UUID(int=number + 1))
    return {
        "type": "assistant",
        "uuid": row_id,
        "sessionId": session,
        "timestamp": "2026-09-16T12:00:00.000Z",
        "message": {
            "id": f"message-{number:05d}",
            "role": "assistant",
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn",
        },
    }


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


def chat_subscription_count(value: object, agent_name: str) -> int:
    return sum(
        record.get("query") != "tail 0"
        for record in recent_subscriptions(value, agent_name)
    )


class Journey:
    def __init__(self, name: str, output: Path, agents: list[tuple[str, str]]):
        self.name = name
        self.output = output
        if output.exists():
            shutil.rmtree(output)
        output.mkdir(parents=True)
        self.scratch_owner = tempfile.TemporaryDirectory(prefix=f"amux-store-{name}-", dir="/tmp")
        self.scratch = Path(self.scratch_owner.name)
        (self.scratch / "tmp").mkdir()
        self.server = f"amux-store-{name}-{os.getpid()}"
        self.frames: list[str] = []
        self.actions: list[str] = []
        self.parts: list[Path] = []
        self.process: subprocess.Popen[str] | None = None
        self.control = ""
        self.config = Path()
        self.store = Path()
        self.ids: dict[str, str] = {}
        self._start(agents)

    def _start(self, agents: list[tuple[str, str]]) -> None:
        declarations = []
        sdk = any(kind == "sdk" for _, kind in agents)
        for name, kind in agents:
            provider: dict[str, object]
            if kind == "sdk":
                provider = {"ClaudeSdk": {"model": "haiku"}}
            else:
                provider = {"Claude": {"script": str(WRITING)}}
            declarations.append(
                {
                    "name": name,
                    "daemon": "host",
                    "working_dir": str(ROOT),
                    "provider": provider,
                }
            )
        daemon: dict[str, object] = {
            "name": "host",
            "user": "alice",
            "repository_roots": [str(ROOT)],
        }
        if sdk:
            daemon["sdk_script"] = str(SDK_SCRIPT)
        topology = {
            "cloud_url": "https://amux.sh",
            "users": ["alice"],
            "daemons": [daemon],
            "paired": [],
            "agents": declarations,
        }
        topology_path = self.scratch / "topology.json"
        topology_path.write_text(json.dumps(topology))
        env = os.environ.copy()
        env.update({key: str(self.scratch / "tmp") for key in ("TMPDIR", "TMP", "TEMP")})
        env["CLAUDE_CONFIG_DIR"] = str(self.scratch / "claude-config")
        self.process = subprocess.Popen(
            [str(TESTNET), "serve", "--topology", str(topology_path)],
            cwd=ROOT,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        ready = read_readiness(self.process)
        self.control = ready["control"]
        daemon_ready = next(item for item in ready["daemons"] if item["name"] == "host")
        self.config = Path(daemon_ready["profile_config"])
        self.store = self.config.parent / "store.sqlite"
        self.ids = {item["name"]: item["agent_id"] for item in ready["agents"]}
        self.actions.append(
            f"daemon ready; profile={self.config}; agents={json.dumps(self.ids, sort_keys=True)}"
        )

    def tmux(self, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["tmux", "-L", self.server, *args],
            check=check,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def capture(self, pane: str) -> str:
        return self.tmux("capture-pane", "-p", "-S", "-80", "-t", pane).stdout

    def wait_frame(self, pane: str, *terms: str, timeout: float = 60.0) -> str:
        deadline = time.monotonic() + timeout
        last = ""
        while time.monotonic() < deadline:
            last = self.capture(pane)
            if all(term in last for term in terms):
                return last
            time.sleep(0.1)
        raise RuntimeError(
            f"{pane} did not show {terms!r}; final frame:\n{last[-4000:]}"
        )

    def page_until(
        self, pane: str, term: str, timeout: float = 90.0, older: bool = True
    ) -> str:
        deadline = time.monotonic() + timeout
        last = self.capture(pane)
        while time.monotonic() < deadline:
            if term in last:
                return last
            if older:
                self.tmux("send-keys", "-t", pane, "C-Home")
                self.tmux("send-keys", "-t", pane, "PPage")
            else:
                self.tmux("send-keys", "-t", pane, "NPage")
            changed_by = min(deadline, time.monotonic() + 1.0)
            while time.monotonic() < changed_by:
                frame = self.capture(pane)
                if term in frame:
                    return frame
                if frame != last:
                    last = frame
                    break
                time.sleep(0.05)
        raise RuntimeError(
            f"{pane} did not page to {term!r}; final frame:\n{last[-4000:]}"
        )

    def launch(self, session: str, config: Path | None = None) -> str:
        config = config or self.config
        part = self.scratch / f"typescript-{len(self.parts) + 1}.txt"
        self.parts.append(part)
        command = " ".join(
            [
                "env",
                "TERM=xterm-256color",
                "AMUX_TUI_DIRECT_PROFILE=1",
                "RUST_LOG=amux=debug,ui_runtime=debug",
                f"AMUX_LOG={shlex.quote(str(self.scratch / f'{session}.log'))}",
                shlex.quote(str(AMUX)),
                "--config",
                shlex.quote(str(config)),
                "ui",
            ]
        )
        recorded = "script -q " + shlex.quote(str(part)) + " sh -c " + shlex.quote(command)
        held = recorded + "; status=$?; echo AMUX_EXIT_$status; sleep 120"
        self.tmux(
            "new-session",
            "-d",
            "-x",
            "120",
            "-y",
            "40",
            "-s",
            session,
            "sh",
            "-c",
            held,
        )
        self.actions.append(f"launch {session}: {command}")
        return session

    def kill(self, session: str) -> None:
        self.tmux("kill-session", "-t", session, check=False)
        self.actions.append(f"close {session}")

    def open_chat(self, pane: str, name: str) -> str:
        self.wait_frame(pane, name)
        self.tmux("send-keys", "-t", pane, "/")
        self.tmux("send-keys", "-t", pane, "-l", name)
        self.wait_frame(pane, f"> {name}")
        self.tmux("send-keys", "-t", pane, "Escape")
        deadline = time.monotonic() + 30
        selected = ""
        while time.monotonic() < deadline:
            selected = self.capture(pane)
            if name in selected and f"> {name}" not in selected:
                break
            time.sleep(0.05)
        else:
            raise RuntimeError(f"{pane} did not leave fleet search:\n{selected[-4000:]}")
        if "o chat" in selected:
            self.tmux("send-keys", "-t", pane, "o")
        elif "enter chat" in selected:
            self.tmux("send-keys", "-t", pane, "Enter")
        else:
            raise RuntimeError(f"{pane} offered no chat entry for {name!r}:\n{selected[-4000:]}")
        return self.wait_frame(pane, name, "Type a message")

    def wait_log(self, session: str, term: str, timeout: float = 60.0) -> str:
        path = self.scratch / f"{session}.log"
        deadline = time.monotonic() + timeout
        last = ""
        while time.monotonic() < deadline:
            if path.exists():
                last = path.read_text(errors="replace")
                if term in last:
                    return last
            time.sleep(0.1)
        raise RuntimeError(f"{session} log did not contain {term!r}; final log:\n{last[-4000:]}")

    def fleet(self, pane: str) -> str:
        self.tmux("send-keys", "-t", pane, "C-a", "s")
        self.wait_frame(pane, "connected")
        self.tmux("send-keys", "-t", pane, "/")
        self.tmux("send-keys", "-t", pane, "C-c")
        self.tmux("send-keys", "-t", pane, "Escape")
        return self.wait_frame(pane, "agents")

    def send(self, pane: str, text: str) -> None:
        self.wait_frame(pane, "enter send")
        self.tmux("send-keys", "-t", pane, "-l", text)
        self.tmux("send-keys", "-t", pane, "Enter")
        self.actions.append(f"{pane}: send {text!r}")

    def frame(self, pane: str, label: str, frame: str | None = None) -> str:
        frame = frame if frame is not None else self.capture(pane)
        self.frames.append(f"FRAME {len(self.frames) + 1} {label} ({pane})\n{frame}")
        return frame

    def control_request(self, request: object, timeout: float = 60.0) -> dict:
        self.actions.append("control: " + json.dumps(request, separators=(",", ":"))[:500])
        return ask(self.control, request, timeout)

    def emit(self, agent: str, rows: list[dict]) -> None:
        self.control_request({"AgentEmit": {"agent": agent, "rows": rows}}, 120)

    def emit_paced(self, agent: str, start: int, count: int, prefix: str) -> None:
        session = self.ids[agent]
        chunk = 100
        for offset in range(0, count, chunk):
            before = time.monotonic()
            rows = [
                row(number, f"{prefix}-{number:05d}", session)
                for number in range(start + offset, start + min(count, offset + chunk))
            ]
            self.emit(agent, rows)
            elapsed = time.monotonic() - before
            time.sleep(max(0.0, len(rows) / 2000.0 - elapsed))

    def dump(self, agent: str) -> str:
        env = os.environ.copy()
        env["AMUX_TUI_DIRECT_PROFILE"] = "1"
        result = subprocess.run(
            [str(AMUX), "--config", str(self.config), "store", "dump", self.ids[agent]],
            cwd=ROOT,
            env=env,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
        )
        return result.stdout

    def wait_dump(self, agent: str, needle: str, timeout: float = 90.0) -> str:
        deadline = time.monotonic() + timeout
        last = ""
        while time.monotonic() < deadline:
            try:
                last = self.dump(agent)
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
                time.sleep(0.2)
                continue
            if needle in last:
                return last
            time.sleep(0.2)
        raise RuntimeError(f"store dump never contained {needle!r}; final dump:\n{last[-4000:]}")

    def diagnostics(self) -> dict:
        return self.control_request(
            {"DebugDump": {"daemon": "host", "verbose": True}}
        )["diagnostics"]

    def finish(self, before: str, after: str, subscriptions: object, assertions: list[str]) -> None:
        (self.output / "frames.txt").write_text("\n\n".join(self.frames) + "\n")
        transcript = "\n".join(self.actions) + "\n"
        for part in self.parts:
            if part.exists():
                transcript += f"\n===== {part.name} =====\n"
                transcript += part.read_text(errors="replace")
        (self.output / "typescript").write_text(transcript)
        (self.output / "dump-before.txt").write_text(before or "not applicable\n")
        (self.output / "dump-after.txt").write_text(after or "not applicable\n")
        (self.output / "subscriptions.txt").write_text(
            json.dumps(subscriptions, indent=2, sort_keys=True) + "\n"
        )
        (self.output / "result.txt").write_text(
            "PASS\n" + "\n".join(f"- {item}" for item in assertions) + "\n"
        )

    def fail(self, error: BaseException) -> None:
        failure = f"FAIL: {error}\n{traceback.format_exc()}"
        (self.output / "result.txt").write_text(failure)
        for name in self.ids:
            try:
                (self.output / f"failure-dump-{name}.txt").write_text(self.dump(name))
            except Exception as dump_error:
                (self.output / f"failure-dump-{name}.txt").write_text(
                    f"dump failed: {dump_error}\n"
                )
        try:
            (self.output / "failure-subscriptions.txt").write_text(
                json.dumps(self.diagnostics(), indent=2, sort_keys=True) + "\n"
            )
        except Exception as diagnostics_error:
            (self.output / "failure-subscriptions.txt").write_text(
                f"diagnostics failed: {diagnostics_error}\n"
            )
        for log in self.scratch.glob("*.log"):
            shutil.copy2(log, self.output / f"failure-{log.name}")
        frames = []
        listed = self.tmux("list-panes", "-a", "-F", "#{session_name}:#{window_index}.#{pane_index}", check=False)
        for pane in listed.stdout.splitlines():
            frames.append(f"{pane}\n{self.capture(pane)}")
        (self.output / "failure-frames.txt").write_text("\n\n".join(frames))

    def close(self) -> None:
        self.tmux("kill-server", check=False)
        if self.process is not None and self.process.poll() is None:
            try:
                ask(self.control, "Shutdown", 15)
            except Exception:
                self.process.terminate()
            try:
                self.process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        if self.process is not None and self.process.stdout is not None:
            remainder = self.process.stdout.read()
            if remainder:
                (self.output / "daemon.log").write_text(remainder)
        self.scratch_owner.cleanup()


def warm_start(output: Path) -> None:
    journey = Journey("warm-start", output, [("remembered-agent", "sdk")])
    try:
        pane = journey.launch("seed")
        journey.open_chat(pane, "remembered-agent")
        journey.wait_frame(pane, "enter send", timeout=90)
        journey.send(pane, "Persist this standing.")
        journey.wait_frame(pane, "The SDK session received your prompt.", timeout=90)
        seed = journey.fleet(pane)
        journey.frame(pane, "online seed", seed)
        journey.wait_dump(
            "remembered-agent", "The SDK session received your prompt.", timeout=90
        )
        journey.kill("seed")
        journey.control_request({"StopDaemon": {"name": "host"}}, 120)
        pane = journey.launch("warm-client")
        remembered = journey.wait_frame(
            pane, "remembered-agent", "remembered", "last finished", "daemon unreachable"
        )
        before = journey.dump("remembered-agent")
        remembered_row = next(
            (line for line in remembered.splitlines() if "remembered-agent" in line), ""
        )
        if "host" not in remembered_row or not re.search(r"\b\d+[smhd]\b", remembered_row):
            raise RuntimeError(
                f"remembered fleet row omitted its host or standing age: {remembered_row!r}"
            )
        journey.frame(pane, "same client paints remembered standing while daemon is stopped", remembered)

        if "o chat" in remembered or "enter chat" in remembered:
            raise RuntimeError("the remembered offline card exposed a send-capable chat action")
        journey.tmux("send-keys", "-t", pane, "o")
        refused = journey.wait_frame(pane, "Type a message", "chat input unavailable")
        journey.tmux("send-keys", "-t", pane, "-l", "must-not-send")
        journey.tmux("send-keys", "-t", pane, "Enter")
        time.sleep(0.5)
        if "must-not-send" in journey.dump("remembered-agent"):
            raise RuntimeError("the unavailable offline composer accepted a send")
        journey.frame(
            pane,
            "daemon-stopped remembered chat shows its send gate is unavailable",
            refused,
        )
        journey.tmux("send-keys", "-t", pane, "C-a", "s")
        journey.wait_frame(pane, "remembered-agent", "daemon unreachable")

        journey.control_request({"RestartSdkDaemon": {"name": "host"}}, 120)
        deadline = time.monotonic() + 90
        confirmed = ""
        while time.monotonic() < deadline:
            confirmed = journey.capture(pane)
            confirmed_row = next(
                (line for line in confirmed.splitlines() if "remembered-agent" in line), ""
            )
            if "connected" in confirmed and confirmed_row and " remembered last " not in confirmed_row:
                break
            time.sleep(0.1)
        else:
            raise RuntimeError(
                "the same client did not replace remembered standing with confirmation:\n"
                + confirmed[-4000:]
            )
        journey.frame(pane, "same client confirms the card after daemon restart", confirmed)
        diagnostics = journey.diagnostics()
        after = journey.dump("remembered-agent")
        journey.finish(
            before,
            after,
            diagnostics,
            [
                "one TUI process stayed alive across the daemon stop and restart",
                "the stopped-daemon frame showed host, age, and last finished standing",
                "the stopped-daemon chat showed an unavailable send gate and persisted no attempted send",
                "the same process replaced remembered state with a confirmed card",
                "dump-before and dump-after came from separate boundary reads",
            ],
        )
    except Exception as error:
        journey.fail(error)
        raise
    finally:
        journey.close()


def chat(output: Path) -> None:
    journey = Journey("chat", output, [("long-chat", "pty")])
    try:
        pane = journey.launch("seed")
        journey.open_chat(pane, "long-chat")
        journey.emit_paced("long-chat", 0, 5000, "stored-row")
        before = journey.wait_dump("long-chat", "stored-row-04999", 180)
        if before.count("entry provider=claude_pty") < 5000:
            raise RuntimeError("the seed dump contains fewer than 5,000 stored entries")
        journey.frame(pane, "5,000 entries stored")
        journey.fleet(pane)
        time.sleep(6)
        journey.kill("seed")

        pane = journey.launch("catch-up")
        journey.open_chat(pane, "long-chat")
        opening = journey.wait_frame(pane, "stored-row-04999", timeout=90)
        if "stored-row-04999" not in opening or "live-row-" in opening:
            raise RuntimeError("the first reopened frame was not an unambiguous store-only paint")
        if "stored-row-00000" in opening:
            raise RuntimeError("the older paging target was already in the loaded window")
        journey.frame(pane, "stored rows paint before any new stream row exists", opening)

        failure: list[BaseException] = []

        def stream() -> None:
            try:
                journey.emit_paced("long-chat", 5000, 6000, "live-row")
            except BaseException as error:  # surfaced on the driver thread below
                failure.append(error)

        producer = threading.Thread(target=stream, name="scripted-2000rps")
        producer.start()
        producer.join(timeout=180)
        if producer.is_alive():
            raise RuntimeError("scripted 2,000 rows/s producer did not finish")
        if failure:
            raise failure[0]
        after = journey.wait_dump("long-chat", "live-row-10999", 180)
        journey.wait_frame(pane, "live-row-10999", timeout=90)
        journey.frame(pane, "caught up at stream tip")
        journey.tmux("send-keys", "-t", pane, "C-Home")
        scrolled = journey.page_until(pane, "stored-row-00000")
        journey.frame(pane, "scroll-back paged older entries", scrolled)
        journey.fleet(pane)
        journey.tmux("send-keys", "-t", pane, "q")
        journey.wait_frame(pane, "AMUX_EXIT_0", timeout=30)
        log = journey.wait_log("catch-up", "store page installed in bounded chat window")
        page_line = next(
            (
                line
                for line in reversed(log.splitlines())
                if "store page installed in bounded chat window" in line
            ),
            "",
        )
        page_fields = {
            name: re.search(rf"\b{name}=(\d+)", page_line)
            for name in ("entries", "encoded_bytes", "max_entries", "max_bytes")
        }
        if not page_line or not all(page_fields.values()):
            raise RuntimeError("the client log did not expose the completed store page read")
        entries, encoded_bytes, max_entries, max_bytes = (
            int(page_fields[name].group(1))
            for name in ("entries", "encoded_bytes", "max_entries", "max_bytes")
        )
        if entries > max_entries or encoded_bytes > max_bytes:
            raise RuntimeError(
                f"paged window exceeded its bound: {entries}/{max_entries} entries, "
                f"{encoded_bytes}/{max_bytes} bytes"
            )
        diagnostics = journey.diagnostics()
        encoded = json.dumps(diagnostics)
        if '"after"' not in encoded and "after " not in encoded:
            raise RuntimeError("subscription diagnostics did not record an after-cursor query")
        journey.finish(
            before,
            after,
            diagnostics,
            [
                "the store held at least 5,000 entries before reopen",
                "the reopened chat painted stored rows before the producer emitted a new row",
                "the client then caught up while a 2,000 rows/s producer ran",
                "scroll-back made a row outside the loaded window visible via a logged store page read",
                f"the visible window remained bounded at {entries}/{max_entries} entries and {encoded_bytes}/{max_bytes} encoded bytes",
                "the caught-up frame showed the final streamed row before scroll-back",
                "subscription diagnostics recorded an exact after-cursor query",
            ],
        )
    except Exception as error:
        journey.fail(error)
        raise
    finally:
        journey.close()


def two_terminal(output: Path) -> None:
    names = ["first-chat", "second-chat", "fleet-only"]
    journey = Journey("two-terminal", output, [(name, "pty") for name in names])
    try:
        one = journey.launch("one")
        journey.wait_frame(one, *names)
        journey.open_chat(one, "first-chat")
        one_only = journey.diagnostics()
        if chat_subscription_count(one_only, "first-chat") != 1:
            raise RuntimeError("terminal one did not open exactly one first-chat subscription")
        if chat_subscription_count(one_only, "second-chat") != 0:
            raise RuntimeError("terminal one subscribed to the unopened second chat")

        two = journey.launch("two")
        journey.wait_frame(two, *names)
        journey.open_chat(two, "second-chat")
        journey.frame(one, "terminal one subscribed to first chat")
        journey.frame(two, "terminal two subscribed to second chat")

        split = journey.diagnostics()
        if chat_subscription_count(split, "first-chat") != 1:
            raise RuntimeError("terminal two changed terminal one's first-chat subscriptions")
        if chat_subscription_count(split, "second-chat") != 1:
            raise RuntimeError("terminal two did not open exactly one second-chat subscription")
        fleet_only = json.dumps(split)
        records = recent_subscriptions(split, "fleet-only")
        if not records or any(record.get("query") != "tail 0" for record in records):
            raise RuntimeError(f"fleet-only opened a client chat subscription: {records}; {fleet_only[:2000]}")

        journey.kill("one")
        one = journey.launch("one-relaunch")
        remembered = journey.wait_frame(one, *names)
        journey.frame(one, "relaunched terminal remembers the fleet without eager chat subscription", remembered)
        relaunched_idle = journey.diagnostics()
        if chat_subscription_count(relaunched_idle, "first-chat") != 1:
            raise RuntimeError("relaunch eagerly resubscribed to its remembered chat")
        if chat_subscription_count(relaunched_idle, "second-chat") != 1:
            raise RuntimeError("relaunch disturbed the other terminal's open chat")
        journey.open_chat(one, "first-chat")
        relaunched_open = journey.diagnostics()
        if chat_subscription_count(relaunched_open, "first-chat") != 2:
            raise RuntimeError("relaunched terminal did not subscribe only when its chat reopened")
        if chat_subscription_count(relaunched_open, "second-chat") != 1:
            raise RuntimeError("relaunched terminal subscribed to the other terminal's chat")

        journey.fleet(one)
        journey.fleet(two)
        journey.control_request(
            {
                "AgentPlay": {
                    "agent": "fleet-only",
                    "steps": [
                        {"Prompt": {"text": "Finish unattended."}},
                        "EndTurn",
                    ],
                }
            }
        )
        journey.frame(one, "both fleets show third agent finished", journey.wait_frame(one, "fleet-only", "finished"))
        journey.frame(two, "second fleet shows third agent finished", journey.wait_frame(two, "fleet-only", "finished"))

        journey.open_chat(one, "first-chat")
        journey.open_chat(two, "first-chat")
        journey.send(one, "writer-one-shared-store")
        journey.wait_dump("first-chat", "writer-one-shared-store")
        journey.wait_frame(two, "writer-one-shared-store")
        journey.control_request({"AgentEndTurn": {"agent": "first-chat"}})
        journey.wait_frame(two, "writer-one-shared-store", "enter send")
        journey.send(two, "writer-two-shared-store")
        after = journey.wait_dump("first-chat", "writer-two-shared-store")
        if after.count("writer-one-shared-store") != 1 or after.count("writer-two-shared-store") != 1:
            raise RuntimeError("the shared transcript did not contain each writer exactly once")
        journey.frame(one, "terminal one after concurrent writes")
        journey.frame(two, "terminal two after concurrent writes")
        diagnostics = journey.diagnostics()
        journey.finish(
            "state none\n",
            after,
            {
                "terminal_one_only": one_only,
                "split_open_chats": split,
                "relaunch_before_open": relaunched_idle,
                "relaunch_after_open": relaunched_open,
                "after_shared_writes": diagnostics,
            },
            [
                "diagnostic deltas attribute terminal one only to first-chat and terminal two only to second-chat",
                "relaunching a terminal with remembered state opened no eager chat subscription",
                "the relaunched terminal subscribed only after first-chat was explicitly reopened",
                "fleet-only had only its daemon summarizer tail-0 subscription",
                "both fleets observed the unopened third agent finish",
                "both writers landed exactly once in one stored transcript",
            ],
        )
    except Exception as error:
        journey.fail(error)
        raise
    finally:
        journey.close()


def gap(output: Path) -> None:
    journey = Journey("gap", output, [("gap-chat", "pty"), ("sdk-survivor", "sdk")])
    try:
        pane = journey.launch("seed")
        journey.open_chat(pane, "sdk-survivor")
        journey.wait_frame(pane, "enter send", timeout=90)
        journey.send(pane, "survivor-provider-entry")
        survivor_before = journey.wait_dump(
            "sdk-survivor", "The SDK session received your prompt.", timeout=90
        )
        journey.fleet(pane)
        journey.open_chat(pane, "gap-chat")
        journey.emit_paced("gap-chat", 0, 30, "old-history")
        before = journey.wait_dump("gap-chat", "old-history-00029")
        journey.frame(pane, "stored history before disconnect")
        journey.fleet(pane)
        time.sleep(6)

        journey.emit_paced("gap-chat", 30, 2200, "offline-row")
        journey.open_chat(pane, "gap-chat")
        journey.tmux("send-keys", "-t", pane, "C-End")
        gap_dump = journey.wait_dump("gap-chat", "boundary=Gap", timeout=90)
        journey.tmux("send-keys", "-t", pane, "C-Home")
        missing = journey.page_until(pane, "missing history")
        journey.frame(pane, "offline past ring shows missing-history boundary", missing)
        if "old-history-00029" not in gap_dump:
            raise RuntimeError("gap recovery discarded the old stored segment")
        journey.fleet(pane)
        time.sleep(6)
        journey.kill("seed")
        pane = journey.launch("gap-reopen")
        journey.open_chat(pane, "gap-chat")
        journey.tmux("send-keys", "-t", pane, "C-Home")
        old = journey.page_until(pane, "old-history-00000", timeout=90)
        journey.frame(pane, "reopened client scrolls stored history behind the gap", old)
        journey.fleet(pane)
        time.sleep(6)

        with sqlite3.connect(journey.store, timeout=30) as database:
            database.execute(
                "UPDATE chat_head SET tip_version=0 WHERE agent_id=?",
                (journey.ids["gap-chat"],),
            )
        journey.open_chat(pane, "gap-chat")
        version_dump = journey.wait_dump("gap-chat", "boundary=VersionGap", timeout=90)
        journey.tmux("send-keys", "-t", pane, "C-End")
        version = journey.wait_frame(pane, "history version changed", timeout=90)
        journey.frame(pane, "tip-version bump keeps history behind boundary", version)
        if "old-history-00029" not in version_dump:
            raise RuntimeError("tip-version recovery discarded stored history")
        journey.fleet(pane)
        time.sleep(6)
        journey.kill("gap-reopen")

        with sqlite3.connect(journey.store, timeout=30) as database:
            database.execute(
                "UPDATE family_shape SET shape=0 WHERE family='claude_pty'"
            )
        pane = journey.launch("entry-version-restart")
        journey.wait_frame(pane, "gap-chat", "sdk-survivor")
        journey.open_chat(pane, "gap-chat")
        time.sleep(1)
        rebuilt_frame = journey.frame(
            pane, "restarted client observes the entry-version rebuild"
        )
        gap_after = journey.dump("gap-chat")
        if "old-history-00029" in gap_after:
            raise RuntimeError("entry-version bump retained an incompatible provider entry")
        journey.fleet(pane)
        survivor_frame = journey.open_chat(pane, "sdk-survivor")
        if "The SDK session received your prompt." not in survivor_frame:
            survivor_frame = journey.wait_frame(
                pane, "The SDK session received your prompt.", timeout=90
            )
        journey.frame(
            pane,
            "other provider remains visible after the restarted client rebuilds Claude PTY",
            survivor_frame,
        )
        survivor_after = journey.dump("sdk-survivor")
        if "The SDK session received your prompt." not in survivor_after:
            raise RuntimeError("entry-version bump discarded the unaffected SDK provider")
        if "The SDK session received your prompt." not in survivor_before:
            raise RuntimeError("the unaffected provider was not stored before the bump")
        after = (
            "===== gap-chat (rebuilt provider) =====\n"
            + gap_after
            + "\n===== sdk-survivor (unaffected provider) =====\n"
            + survivor_after
        )
        diagnostics = journey.diagnostics()
        journey.finish(
            before,
            after,
            diagnostics,
            [
                "falling more than the 2,000-row PTY ring behind showed a gap boundary",
                "old stored history remained behind the gap",
                "a tip-version bump showed a version boundary and kept entries",
                "a restarted client, not the dump command, observed the entry-shape bump",
                "the bump removed only incompatible Claude PTY entries",
                "the second SDK provider remained visible and present in dump-after",
            ],
        )
    except Exception as error:
        journey.fail(error)
        raise
    finally:
        journey.close()


def real_claude_transcript(journey: Journey, session: str) -> list[dict]:
    claude = shutil.which("claude")
    if claude is None:
        raise RuntimeError("the real Claude Code binary is unavailable")
    target = journey.scratch / "permission-request-target.txt"
    prompt = (
        "Create a small resume-history fixture. First call TaskCreate with subject "
        "'Resume from the live checklist', description 'Fixture task for live capture test', "
        "and activeForm 'Resuming from the live checklist'. Then call TaskUpdate for the "
        "created task with status in_progress. Then attempt "
        f"to use the Write tool to write LIVE_PERMISSION_REQUEST to {target}. The permission "
        "may be denied; do not use another tool instead. After all three tool attempts, reply "
        "exactly SDK_LIVE_CAPTURE_OK."
    )
    command = [
        claude,
        "-p",
        "--safe-mode",
        "--model",
        "haiku",
        "--session-id",
        session,
        "--tools",
        "TaskCreate,TaskUpdate,Write",
        "--strict-mcp-config",
        "--mcp-config",
        '{"mcpServers":{}}',
        "--permission-mode",
        "manual",
        "--permission-prompts",
        "none",
        "--max-budget-usd",
        "0.25",
        "--output-format",
        "json",
        prompt,
    ]
    env = os.environ.copy()
    env.pop("CLAUDE_CONFIG_DIR", None)
    completed = subprocess.run(
        command,
        cwd=ROOT,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=240,
    )
    journey.actions.append("real Claude Code capture: " + shlex.join(command[:-1]) + " <prompt>")
    journey.actions.append("real Claude Code output: " + completed.stdout.strip())
    if completed.returncode != 0 or "Not logged in" in completed.stdout:
        raise RuntimeError(
            "the real Claude Code capture could not authenticate; do not substitute scripted rows: "
            + completed.stdout[-2000:]
        )

    config_root = Path.home() / ".claude"
    candidates = list((config_root / "projects").glob(f"**/{session}.jsonl"))
    if len(candidates) != 1:
        raise RuntimeError(
            f"real Claude Code did not leave one transcript for {session}: {candidates}"
        )
    source = candidates[0]
    rows = [json.loads(line) for line in source.read_text().splitlines() if line.strip()]
    tool_uses = [
        block
        for item in rows
        for block in (
            item.get("message", {}).get("content", [])
            if isinstance(item.get("message"), dict)
            else []
        )
        if isinstance(block, dict) and block.get("type") == "tool_use"
    ]
    by_name = {
        name: [block for block in tool_uses if block.get("name") == name]
        for name in ("TaskCreate", "TaskUpdate", "Write")
    }
    if any(not blocks for blocks in by_name.values()):
        raise RuntimeError(
            "the authenticated Claude Code binary did not produce the required task and "
            "permission tools; observed tools were "
            f"{sorted({block.get('name') for block in tool_uses if block.get('name')})}"
        )
    create_input = by_name["TaskCreate"][0].get("input", {})
    if create_input.get("subject") != "Resume from the live checklist" or not create_input.get(
        "activeForm"
    ):
        raise RuntimeError(f"TaskCreate did not carry the live checklist fields: {create_input}")
    update_input = by_name["TaskUpdate"][0].get("input", {})
    if not update_input.get("taskId") or update_input.get("status") != "in_progress":
        raise RuntimeError(f"TaskUpdate did not activate the created task: {update_input}")
    write_input = by_name["Write"][0].get("input", {})
    if write_input.get("file_path") != str(target):
        raise RuntimeError(f"Write did not request the capture target: {write_input}")
    if "SDK_LIVE_CAPTURE_OK" not in source.read_text():
        raise RuntimeError("the real Claude Code session did not finish its capture marker")
    source.unlink()
    return rows


def sdk_resume(output: Path) -> None:
    journey = Journey("sdk-resume", output, [("sdk-history", "sdk")])
    try:
        pane = journey.launch("sdk")
        journey.open_chat(pane, "sdk-history")
        journey.frame(pane, "fresh SDK session before resume")

        session = journey.ids["sdk-history"]
        historical = real_claude_transcript(journey, session)
        slug = "".join(
            character if character.isalnum() and character.isascii() else "-"
            for character in str(ROOT.resolve())
        )
        transcript = journey.scratch / "claude-config" / "projects" / slug / f"{session}.jsonl"
        transcript.parent.mkdir(parents=True, exist_ok=True)
        transcript.write_text("".join(json.dumps(item) + "\n" for item in historical))

        resumed = journey.control_request({"SuspendRestart": {"name": "host"}}, 120)["diagnostics"]
        if resumed != {"resumed": 1, "failed": 0}:
            raise RuntimeError(f"SDK resume counts were not 1/0: {resumed}")
        journey.fleet(pane)
        journey.open_chat(pane, "sdk-history")
        history = journey.wait_frame(pane, "SDK_LIVE_CAPTURE_OK", timeout=90)
        journey.frame(pane, "real Claude Code transcript restored on resume", history)
        before = journey.wait_dump("sdk-history", "SDK_LIVE_CAPTURE_OK", timeout=90)
        if 'todo=0/1 current="Resuming from the live checklist"' not in before:
            raise RuntimeError("historical task tools did not restore the checklist")
        diagnostics = journey.diagnostics()
        if "Write" not in history or '"pending_permissions": 0' not in json.dumps(diagnostics):
            raise RuntimeError("the historical permission was absent or remained answerable")
        if any(term in history for term in ("allow once", "allow always", "deny request")):
            raise RuntimeError("the historical permission rendered live answer controls")

        transcript.unlink()
        if transcript.exists():
            raise RuntimeError("the real Claude Code transcript could not be removed")
        resumed_without_file = journey.control_request(
            {"SuspendRestart": {"name": "host"}}, 120
        )["diagnostics"]
        if resumed_without_file != {"resumed": 1, "failed": 0}:
            raise RuntimeError(
                f"SDK missing-file resume counts were not 1/0: {resumed_without_file}"
            )
        journey.fleet(pane)
        journey.open_chat(pane, "sdk-history")
        stored = journey.wait_frame(pane, "SDK_LIVE_CAPTURE_OK", timeout=90)
        after = journey.wait_dump("sdk-history", "SDK_LIVE_CAPTURE_OK", timeout=90)
        if "SDK_LIVE_CAPTURE_OK" not in after:
            raise RuntimeError("removing the source transcript discarded stored history")
        journey.frame(
            pane,
            "second resume has no source transcript but stored history still paints",
            stored,
        )
        journey.finish(
            before,
            after,
            diagnostics,
            [
                "the claude binary produced the TaskCreate, TaskUpdate and Write permission rows itself",
                "the real historical task tools restored their active checklist visibly",
                "the old Write permission painted without answer controls or a pending obligation",
                "after the source file was removed, a second SDK resume still painted stored history",
            ],
        )
    except Exception as error:
        journey.fail(error)
        raise
    finally:
        journey.close()


RUNNERS = {
    "warm-start": warm_start,
    "chat": chat,
    "two-terminal": two_terminal,
    "gap": gap,
    "sdk-resume": sdk_resume,
}


def main() -> int:
    scenario = sys.argv[1]
    root = Path(sys.argv[2]).resolve()
    root.mkdir(parents=True, exist_ok=True)
    selected = SCENARIOS if scenario == "all" else (scenario,)
    for name in selected:
        print(f"store scenario {name}: starting", flush=True)
        try:
            output = root / name if scenario == "all" else root
            RUNNERS[name](output)
        except Exception as error:
            print(f"store scenario {name}: FAIL: {error}", file=sys.stderr, flush=True)
            return 1
        print(f"store scenario {name}: PASS", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

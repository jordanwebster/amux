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
REAL_SDK_CAPTURE = (
    ROOT
    / "crates/agent-runtime/src/agents/claude/fixtures/sdk-resume-identity.json"
)
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

    def page_until(self, pane: str, term: str, timeout: float = 90.0) -> str:
        deadline = time.monotonic() + timeout
        last = self.capture(pane)
        while time.monotonic() < deadline:
            if term in last:
                return last
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
    journey = Journey("warm-start", output, [("remembered-agent", "pty")])
    try:
        pane = journey.launch("seed")
        journey.wait_frame(pane, "remembered-agent")
        journey.control_request(
            {
                "AgentPlay": {
                    "agent": "remembered-agent",
                    "steps": [
                        {"Prompt": {"text": "Persist this standing."}},
                        {"Markdown": {"text": "Standing is stored."}},
                        "EndTurn",
                    ],
                }
            }
        )
        journey.wait_frame(pane, "remembered-agent", "finished")
        journey.frame(pane, "online seed")
        journey.kill("seed")

        offline = journey.scratch / "offline.yaml"
        text = journey.config.read_text()
        text = re.sub(
            r"(?m)^socket_path:.*$",
            f"socket_path: '{journey.scratch / 'unreachable.sock'}'",
            text,
        )
        offline.write_text(text)
        pane = journey.launch("offline", offline)
        remembered = journey.wait_frame(pane, "remembered-agent", "remembered")
        if "disconnected" not in remembered:
            raise RuntimeError("offline remembered fleet did not report its disconnected route")
        journey.frame(pane, "daemon unreachable remembered fleet", remembered)
        journey.kill("offline")

        pane = journey.launch("confirmed")
        confirmed = journey.wait_frame(pane, "remembered-agent", "finished")
        journey.frame(pane, "daemon reachable confirmed fleet", confirmed)
        diagnostics = journey.diagnostics()
        dump = journey.dump("remembered-agent")
        journey.finish(
            dump,
            dump,
            diagnostics,
            [
                "the unreachable profile painted its remembered fleet row",
                "the remembered frame stayed visible while the route was disconnected",
                "the reachable profile confirmed the stored standing",
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

        failure: list[BaseException] = []

        def stream() -> None:
            try:
                journey.emit_paced("long-chat", 5000, 6000, "live-row")
            except BaseException as error:  # surfaced on the driver thread below
                failure.append(error)

        producer = threading.Thread(target=stream, name="scripted-2000rps")
        producer.start()
        pane = journey.launch("catch-up")
        opening = journey.open_chat(pane, "long-chat")
        journey.frame(pane, "disk paint while scripted stream runs", opening)
        producer.join(timeout=180)
        if producer.is_alive():
            raise RuntimeError("scripted 2,000 rows/s producer did not finish")
        if failure:
            raise failure[0]
        after = journey.wait_dump("long-chat", "live-row-10999", 180)
        journey.wait_frame(pane, "live-row-10999", timeout=90)
        journey.frame(pane, "caught up at stream tip")
        journey.tmux("send-keys", "-t", pane, "C-Home")
        scrolled = journey.page_until(pane, "stored-row-04599")
        journey.frame(pane, "scroll-back paged older entries", scrolled)
        journey.tmux("send-keys", "-t", pane, "C-End")
        returned = journey.wait_frame(pane, "live-row-10999", timeout=90)
        journey.frame(pane, "returned to the streamed tip", returned)
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
                "the reopened chat painted while a 2,000 rows/s producer ran",
                "the chat paged to a row older than its initial 400-entry window",
                "returning to the tip restored the final streamed row",
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
        two = journey.launch("two")
        for pane in (one, two):
            journey.wait_frame(pane, *names)
        journey.open_chat(one, "first-chat")
        journey.open_chat(two, "second-chat")
        journey.frame(one, "terminal one subscribed to first chat")
        journey.frame(two, "terminal two subscribed to second chat")

        diagnostics = journey.diagnostics()
        fleet_only = json.dumps(diagnostics)
        records = recent_subscriptions(diagnostics, "fleet-only")
        if not records or any(record.get("query") != "tail 0" for record in records):
            raise RuntimeError(f"fleet-only opened a client chat subscription: {records}; {fleet_only[:2000]}")

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
            diagnostics,
            [
                "each terminal initially subscribed only to its open chat",
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
    journey = Journey("gap", output, [("gap-chat", "pty")])
    try:
        pane = journey.launch("seed")
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
        old = journey.wait_frame(pane, "old-history-00000", timeout=90)
        journey.frame(pane, "old stored history remains scrollable behind the gap", old)
        missing = journey.page_until(pane, "missing history")
        journey.frame(pane, "offline past ring shows missing-history boundary", missing)
        if "old-history-00029" not in gap_dump:
            raise RuntimeError("gap recovery discarded the old stored segment")
        journey.fleet(pane)
        time.sleep(6)

        with sqlite3.connect(journey.store, timeout=30) as database:
            database.execute(
                "UPDATE chat_head SET tip_version=0 WHERE agent_id=?",
                (journey.ids["gap-chat"],),
            )
        journey.open_chat(pane, "gap-chat")
        version_dump = journey.wait_dump("gap-chat", "boundary=VersionGap", timeout=90)
        journey.tmux("send-keys", "-t", pane, "C-Home")
        version = journey.page_until(pane, "history version changed")
        journey.frame(pane, "tip-version bump keeps history behind boundary", version)
        if "old-history-00029" not in version_dump:
            raise RuntimeError("tip-version recovery discarded stored history")
        journey.fleet(pane)
        time.sleep(6)

        with sqlite3.connect(journey.store, timeout=30) as database:
            database.execute(
                "UPDATE family_shape SET shape=0 WHERE family='claude_pty'"
            )
        journey.open_chat(pane, "gap-chat")
        time.sleep(1)
        journey.frame(pane, "entry-version bump rebuilds only provider entries")
        after = journey.dump("gap-chat")
        if "old-history-00029" in after:
            raise RuntimeError("entry-version bump retained an incompatible provider entry")
        diagnostics = journey.diagnostics()
        journey.finish(
            before,
            after,
            diagnostics,
            [
                "falling more than the 2,000-row PTY ring behind showed a gap boundary",
                "old stored history remained behind the gap",
                "a tip-version bump showed a version boundary and kept entries",
                "an entry-shape bump removed the incompatible provider entries",
            ],
        )
    except Exception as error:
        journey.fail(error)
        raise
    finally:
        journey.close()


def sdk_resume(output: Path) -> None:
    if not REAL_SDK_CAPTURE.is_file():
        raise RuntimeError(f"real Claude Code capture is missing: {REAL_SDK_CAPTURE}")
    journey = Journey("sdk-resume", output, [("sdk-history", "sdk")])
    try:
        pane = journey.launch("sdk")
        journey.open_chat(pane, "sdk-history")
        journey.frame(pane, "fresh SDK session before resume")

        captured = json.loads(REAL_SDK_CAPTURE.read_text())
        real_row = captured["transcript_row"]
        session = journey.ids["sdk-history"]
        slug = "".join(
            character if character.isalnum() and character.isascii() else "-"
            for character in str(ROOT.resolve())
        )
        transcript = journey.scratch / "claude-config" / "projects" / slug / f"{session}.jsonl"
        transcript.parent.mkdir(parents=True, exist_ok=True)
        real_row = json.loads(json.dumps(real_row))
        real_row["sessionId"] = session
        historical = [
            {
                "type": "assistant",
                "uuid": "00000000-0000-0000-0000-00000000aa01",
                "sessionId": session,
                "timestamp": "2026-09-16T10:00:00.000Z",
                "message": {
                    "id": "todo-history-message",
                    "role": "assistant",
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "todo-history",
                            "name": "TodoWrite",
                            "input": {
                                "todos": [
                                    {
                                        "content": "Resume from the stored checklist",
                                        "status": "in_progress",
                                        "activeForm": "Resuming from the stored checklist",
                                    }
                                ]
                            },
                        }
                    ],
                },
            },
            {
                "type": "user",
                "uuid": "00000000-0000-0000-0000-00000000aa03",
                "sessionId": session,
                "timestamp": "2026-09-16T10:00:00.500Z",
                "message": {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "todo-history",
                            "content": "Todos updated",
                            "is_error": False,
                        }
                    ],
                },
            },
            {
                "type": "assistant",
                "uuid": "00000000-0000-0000-0000-00000000aa02",
                "sessionId": session,
                "timestamp": "2026-09-16T10:00:01.000Z",
                "message": {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "old-permission",
                            "name": "Write",
                            "input": {"file_path": "/tmp/old", "content": "old"},
                        }
                    ],
                },
            },
            real_row,
        ]
        transcript.write_text("".join(json.dumps(item) + "\n" for item in historical))

        resumed = journey.control_request({"SuspendRestart": {"name": "host"}}, 120)["diagnostics"]
        if resumed != {"resumed": 1, "failed": 0}:
            raise RuntimeError(f"SDK resume counts were not 1/0: {resumed}")
        journey.fleet(pane)
        journey.open_chat(pane, "sdk-history")
        history = journey.wait_frame(pane, "SDK_PROMPT_OK", timeout=90)
        journey.frame(pane, "real Claude Code transcript restored on resume", history)
        before = journey.wait_dump("sdk-history", "SDK_PROMPT_OK", timeout=90)
        if 'todo=0/1 current="Resuming from the stored checklist"' not in before:
            raise RuntimeError("historical TodoWrite did not restore the checklist")
        diagnostics = journey.diagnostics()
        if "tool:old-permission" not in before or '"pending_permissions": 0' not in json.dumps(diagnostics):
            raise RuntimeError("the historical permission was absent or remained answerable")

        transcript.unlink()
        if transcript.exists():
            raise RuntimeError("the real Claude Code transcript could not be removed")
        after = journey.dump("sdk-history")
        if "SDK_PROMPT_OK" not in after:
            raise RuntimeError("removing the source transcript discarded stored history")
        journey.frame(
            pane,
            "source transcript removed after resume; stored history remains readable",
        )
        journey.finish(
            before,
            after,
            diagnostics,
            [
                "a transcript row captured from real Claude Code painted after SDK resume",
                "the historical TodoWrite restored its checklist",
                "the old permission was history rather than an answerable obligation",
                "the source transcript was removed after resume without discarding stored history",
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

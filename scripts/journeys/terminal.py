"""Reusable real-terminal journey driver.

Scenario modules supply acts and assertions. This module owns process,
terminal, door, evidence, and golden mechanics only.
"""

from __future__ import annotations

from dataclasses import dataclass
import difflib
import json
import os
from pathlib import Path
import re
import select
import shlex
import shutil
import socket
import subprocess
import tempfile
import time
from typing import Callable

ROOT = Path(__file__).resolve().parents[2]
AMUX = ROOT / "target/debug/amux"
TESTNET = ROOT / "target/debug/testnet"
TEST_AGENT = ROOT / "target/debug/test-agent"
MANIFEST = ROOT / "journeys/manifest.json"
OUTPUT = ROOT / "target/journeys"
GOLDENS = ROOT / "journeys/goldens/terminal"


def door(address: str, request: object, timeout: float = 60.0) -> dict:
    host, port = address.rsplit(":", 1)
    with socket.create_connection((host, int(port)), timeout=timeout) as connection:
        connection.settimeout(timeout)
        connection.sendall((json.dumps(request, separators=(",", ":")) + "\n").encode())
        with connection.makefile("rb") as stream:
            encoded = stream.readline()
    try:
        reply = json.loads(encoded)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"invalid door reply {encoded!r}") from error
    if "Ack" not in reply:
        raise RuntimeError(f"door refused {request!r}: {reply!r}")
    return reply["Ack"]


def _read_readiness(process: subprocess.Popen[bytes], timeout: float = 60.0) -> dict:
    assert process.stdout is not None
    deadline = time.monotonic() + timeout
    seen: list[str] = []
    buffered = b""
    while time.monotonic() < deadline:
        remaining = deadline - time.monotonic()
        readable, _, _ = select.select([process.stdout], [], [], max(0.0, remaining))
        if not readable:
            break
        chunk = os.read(process.stdout.fileno(), 4096)
        if not chunk:
            if process.poll() is not None:
                raise RuntimeError(
                    f"testnet exited before readiness ({process.returncode}): {seen!r}"
                )
            continue
        buffered += chunk
        while b"\n" in buffered:
            encoded, buffered = buffered.split(b"\n", 1)
            line = encoded.decode(errors="replace")
            try:
                value = json.loads(line)
            except json.JSONDecodeError:
                seen.append(line)
                continue
            if isinstance(value, dict) and "control" in value and "daemons" in value:
                return value
            seen.append(line)
    raise RuntimeError(f"testnet readiness timed out: {seen!r}")


def _stable_frame(
    text: str,
    styles: str,
    *,
    normalize_session_idle: bool = False,
) -> tuple[str, str]:
    """Exclude transient clocks and report paths without changing geometry."""
    # Report frames delimit terminal rows with LF. `str.splitlines()` also
    # treats several Unicode screen-cell values as separators, which can make
    # equal-height text and style maps appear to have different geometry.
    text_rows = text.removesuffix("\n").split("\n")
    style_rows = styles.removesuffix("\n").split("\n")
    if len(text_rows) != len(style_rows):
        raise RuntimeError(
            "captured frame text and semantic styles have different heights: "
            f"text={len(text_rows)} styles={len(style_rows)}"
        )
    marker = "─ turn · "
    for index, row in enumerate(text_rows):
        def stable_age(match: re.Match[str]) -> str:
            width = len(match.group("age")) + len(match.group("space"))
            return "<age>" + " " * (width - len("<age>"))

        row = re.sub(
            r"(?P<age>\d+(?:ms|s|m|h))(?P<space> {2,})(?=(?:idle|working|–))",
            stable_age,
            row,
        )
        if normalize_session_idle and "chat · idle" in row:
            start = row.rindex("chat · idle")
            if start < 3 or row[start - 3 : start] != "   ":
                raise RuntimeError(f"session header has no normalization space: {row!r}")
            row = row[: start - 3] + "default · idle" + row[start + len("chat · idle") :]
        text_rows[index] = row
        report_marker = "✔ wrote "
        if report_marker in row:
            start = row.index(report_marker)
            end = len(row) - 1
            replacement = "✔ report written"
            if end - start < len(replacement):
                raise RuntimeError(f"report row is too narrow to normalize: {row!r}")
            text_rows[index] = (
                row[:start] + replacement + " " * (end - start - len(replacement)) + row[end:]
            )
        if marker not in row:
            continue
        start = row.index(marker) + len(marker)
        replacement = "<elapsed> "
        remainder = len(row) - start - len(replacement)
        if remainder < 1:
            raise RuntimeError(f"turn row has no rule after its clock: {row!r}")
        text_rows[index] = row[:start] + replacement + "─" * remainder
        style = style_rows[index]
        if len(style) != len(row):
            raise RuntimeError("captured frame row and semantic styles have different widths")
        style_rows[index] = (
            style[:start]
            + style[start] * len(replacement)
            + style[-1] * remainder
        )
    return "\n".join(text_rows) + "\n", "\n".join(style_rows) + "\n"


@dataclass(frozen=True)
class Frame:
    label: str
    text: str
    styles: str


class TerminalJourney:
    def __init__(self, story: dict):
        self.story = story
        self.name = story["id"]
        self.output = OUTPUT / self.name
        if self.output.exists():
            shutil.rmtree(self.output)
        self.output.mkdir(parents=True)
        self.scratch_owner = tempfile.TemporaryDirectory(prefix=f"amux-journey-{self.name}-")
        self.scratch = Path(self.scratch_owner.name)
        self.server = f"amux-journey-{os.getpid()}-{self.name}"
        self.process: subprocess.Popen[bytes] | None = None
        self.control = ""
        self.config = Path()
        self.reports = Path()
        self.frames: list[Frame] = []
        self.actions: list[str] = []
        self.observations: dict[str, object] = {}
        try:
            self._start()
        except BaseException:
            if self.process is not None and self.process.poll() is None:
                self.process.kill()
                self.process.wait(timeout=30)
            self.scratch_owner.cleanup()
            raise

    def _start(self) -> None:
        topology = ROOT / self.story["topology"]
        env = os.environ.copy()
        env.update({key: str(self.scratch) for key in ("TMPDIR", "TMP", "TEMP")})
        self.process = subprocess.Popen(
            [str(TESTNET), "serve", "--topology", str(topology)],
            cwd=ROOT,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
        )
        ready = _read_readiness(self.process)
        self.control = ready["control"]
        daemon = next(item for item in ready["daemons"] if item["name"] == "terminal-host")
        self.config = Path(daemon["profile_config"])
        data_dir = next(
            Path(line.split(": ", 1)[1])
            for line in self.config.read_text().splitlines()
            if line.startswith("data_dir: ")
        )
        self.reports = data_dir / "reports"
        self.actions.append(f"ready control={self.control} config={self.config}")

    def tmux(self, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["tmux", "-L", self.server, *args],
            check=check,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
        )

    def launch(self, name: str) -> str:
        command = " ".join(
            [
                "env",
                "TERM=xterm-256color",
                # Terminal goldens are approved in ANSI mode. Hosted macOS
                # exports truecolor here while Linux and local tmux do not.
                "COLORTERM=",
                shlex.quote(str(AMUX)),
                "--config",
                shlex.quote(str(self.config)),
                "ui",
            ]
        )
        held = command + "; status=$?; echo AMUX_EXIT_$status; sleep 120"
        self.tmux("new-session", "-d", "-x", "120", "-y", "40", "-s", name, "sh", "-c", held)
        self.actions.append(f"launch {name}: {command}")
        return name

    def launch_agent(self, pane: str, agent: str) -> str:
        command = " ".join(
            [
                "env",
                "TERM=xterm-256color",
                "COLORTERM=",
                shlex.quote(str(AMUX)),
                "--config",
                shlex.quote(str(self.config)),
                "new",
                shlex.quote(str(TEST_AGENT)),
                "--name",
                shlex.quote(agent),
            ]
        )
        held = command + "; status=$?; echo AMUX_EXIT_$status; sleep 120"
        self.tmux("new-session", "-d", "-x", "120", "-y", "40", "-s", pane, "sh", "-c", held)
        self.actions.append(f"launch {pane}: {command}")
        return pane

    def capture(self, pane: str) -> str:
        return self.tmux("capture-pane", "-p", "-J", "-t", pane).stdout

    def wait(
        self,
        pane: str,
        predicate: Callable[[str], bool],
        description: str,
        timeout: float = 60.0,
    ) -> str:
        deadline = time.monotonic() + timeout
        last = ""
        while time.monotonic() < deadline:
            last = self.capture(pane)
            if predicate(last):
                self.actions.append(f"{pane}: reached {description}")
                return last
            time.sleep(0.1)
        raise RuntimeError(f"timed out waiting for {description}; final frame:\n{last}")

    def wait_terms(self, pane: str, *terms: str, timeout: float = 60.0) -> str:
        return self.wait(
            pane,
            lambda frame: all(term in frame for term in terms),
            repr(terms),
            timeout,
        )

    def keys(self, pane: str, *keys: str) -> None:
        self.tmux("send-keys", "-t", pane, *keys)
        self.actions.append(f"{pane}: keys {' '.join(keys)}")

    def type(self, pane: str, value: str) -> None:
        self.tmux("send-keys", "-t", pane, "-l", value)
        self.actions.append(f"{pane}: type {value!r}")

    def select_agent(self, pane: str, agent: str) -> str:
        self.wait_terms(pane, agent)
        self.keys(pane, "Escape")
        self.keys(pane, "g", "g")
        last = ""
        for _ in range(100):
            time.sleep(0.1)
            last = self.capture(pane)
            if any(
                agent in line and "▎" in line.partition(agent)[0]
                for line in last.splitlines()
            ):
                self.actions.append(f"{pane}: selected fleet agent {agent}")
                return last
            self.keys(pane, "j")
        raise RuntimeError(f"could not select fleet agent {agent}; final frame:\n{last}")

    def filter_agent(self, pane: str, agent: str) -> str:
        self.wait_terms(pane, agent)
        self.keys(pane, "Escape")
        self.keys(pane, "/")
        self.keys(pane, "C-u")
        self.type(pane, agent)
        self.wait(
            pane,
            lambda frame: f"> {agent}" in frame
            and any(
                agent in line and "▎" in line.partition(agent)[0]
                for line in frame.splitlines()
            ),
            f"filtered fleet agent {agent}",
        )
        self.keys(pane, "Escape")
        return self.wait_terms(pane, f"/ {agent}")

    def clear_filter(self, pane: str) -> str:
        self.keys(pane, "/")
        self.keys(pane, "C-c")
        self.keys(pane, "Escape")
        return self.wait(
            pane,
            lambda frame: " agents" in frame and "1/" not in frame,
            "unfiltered fleet",
        )

    def open_chat(self, pane: str, agent: str) -> None:
        selected = self.select_agent(pane, agent)
        self.keys(pane, "o" if "o chat" in selected else "Enter")
        self.wait_terms(pane, agent, "Type a message")

    def request(self, request: object, label: str | None = None) -> dict:
        self.actions.append("door " + json.dumps(request, separators=(",", ":")))
        reply = door(self.control, request)
        if label is not None:
            self.observations[label] = reply
        return reply

    def observe(self, agent: str, label: str) -> list[dict]:
        observed = self.request({"AgentObserve": {"agent": agent}}, label).get("observed", [])
        if not isinstance(observed, list):
            raise RuntimeError(f"invalid observation for {agent}: {observed!r}")
        return observed

    def wait_observation(
        self,
        agent: str,
        predicate: Callable[[list[dict]], bool],
        description: str,
        timeout: float = 60.0,
    ) -> list[dict]:
        deadline = time.monotonic() + timeout
        last: list[dict] = []
        while time.monotonic() < deadline:
            last = self.observe(agent, f"poll-{description}")
            if predicate(last):
                self.observations[description] = last
                return last
            time.sleep(0.1)
        raise RuntimeError(f"timed out waiting for {description}; observed {last!r}")

    def frame(
        self,
        pane: str,
        label: str,
        *,
        normalize_session_idle: bool = False,
    ) -> Frame:
        before = set(self.reports.iterdir()) if self.reports.exists() else set()
        self.keys(pane, "C-g")
        self.wait_terms(pane, "report this screen:", "b bug", "t tweak")
        self.keys(pane, "b")
        self.wait_terms(pane, "what happened?")
        self.type(pane, f"terminal journey {self.name} {label}")
        self.keys(pane, "Enter")
        self.wait_terms(pane, "marked 0:", "enter finish")
        self.keys(pane, "Enter")
        deadline = time.monotonic() + 60
        report = None
        while time.monotonic() < deadline:
            current = set(self.reports.iterdir()) if self.reports.exists() else set()
            complete = [
                path
                for path in current - before
                if (path / "frame.txt").is_file() and (path / "frame.styles").is_file()
            ]
            if len(complete) == 1:
                report = complete[0]
                break
            if len(complete) > 1:
                raise RuntimeError(f"one frame capture wrote multiple reports: {complete!r}")
            time.sleep(0.1)
        if report is None:
            raise RuntimeError(f"timed out waiting for the {label!r} frame report")
        text, styles = _stable_frame(
            (report / "frame.txt").read_text(),
            (report / "frame.styles").read_text(),
            normalize_session_idle=normalize_session_idle,
        )
        self.actions.append(f"captured {label} from {report.name}")
        frame = Frame(label, text, styles)
        self.frames.append(frame)
        actual = self.output / "actual"
        actual.mkdir(exist_ok=True)
        (actual / f"{label}.txt").write_text(text)
        (actual / f"{label}.styles").write_text(styles)
        self._compare(frame)
        return frame

    def _compare(self, frame: Frame) -> None:
        golden_dir = GOLDENS / self.name
        golden_dir.mkdir(parents=True, exist_ok=True)
        expected = {
            golden_dir / f"{frame.label}.txt": frame.text,
            golden_dir / f"{frame.label}.styles": frame.styles,
        }
        update = os.environ.get("UPDATE_JOURNEY_GOLDENS") == "1"
        for path, actual in expected.items():
            if update:
                path.write_text(actual)
            elif not path.exists():
                raise RuntimeError(
                    f"missing journey golden {path}; "
                    "review with UPDATE_JOURNEY_GOLDENS=1"
                )
            else:
                approved = path.read_text()
                if approved == actual:
                    continue
                diff = "\n".join(
                    difflib.unified_diff(
                        approved.splitlines(),
                        actual.splitlines(),
                        fromfile=str(path),
                        tofile=f"actual/{path.name}",
                        lineterm="",
                    )
                )
                raise RuntimeError(f"journey golden differs: {path}\n{diff}")

    def stop_client(self, pane: str) -> None:
        self.keys(pane, "C-a", "s")
        self.wait(
            pane,
            lambda frame: "┌ amux" in frame and "Type a message" not in frame,
            "fleet before quit",
        )
        self.keys(pane, "q")
        self.wait_terms(pane, "AMUX_EXIT_0", timeout=30)

    def kill_client(self, pane: str) -> None:
        self.tmux("kill-session", "-t", pane, check=False)
        self.actions.append(f"terminate {pane}")

    def finish(self, assertions: list[str]) -> None:
        (self.output / "actions.txt").write_text("\n".join(self.actions) + "\n")
        observations = json.dumps(self.observations, indent=2, sort_keys=True) + "\n"
        (self.output / "observations.json").write_text(observations)
        result = "PASS\n" + "\n".join(f"- {item}" for item in assertions) + "\n"
        (self.output / "result.txt").write_text(result)

    def fail(self, error: BaseException) -> None:
        observations = json.dumps(self.observations, indent=2, sort_keys=True) + "\n"
        (self.output / "observations.json").write_text(observations)
        (self.output / "result.txt").write_text(f"FAIL\n- {type(error).__name__}: {error}\n")

    def close(self) -> None:
        try:
            self.tmux("kill-server", check=False)
            if self.process is not None and self.process.poll() is None:
                try:
                    door(self.control, {"StopDaemon": {"name": "terminal-host"}}, 15)
                except Exception:
                    pass
                self.process.kill()
                self.process.wait(timeout=30)
            if self.process is not None and self.process.stdout is not None:
                remainder = self.process.stdout.read().decode(errors="replace")
                if remainder:
                    (self.output / "testnet.log").write_text(remainder)
            try:
                door(self.control, {"Inventory": {"daemon": "terminal-host"}}, 0.2)
            except OSError:
                self.actions.append("teardown: control socket closed")
            else:
                raise RuntimeError("testnet control socket remained reachable after teardown")
            (self.output / "actions.txt").write_text("\n".join(self.actions) + "\n")
        finally:
            self.scratch_owner.cleanup()


def story(name: str) -> dict:
    manifest = json.loads(MANIFEST.read_text())
    matches = [item for item in manifest["journeys"] if item["id"] == name]
    if len(matches) != 1 or "terminal" not in matches[0].get("clients", []):
        raise RuntimeError(f"no terminal journey named {name!r}")
    return matches[0]

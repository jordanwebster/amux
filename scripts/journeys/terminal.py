"""The terminal journey driver.

Starts a served topology with `testnet serve`, launches the real `amux`
terminal client against one host's install in a private tmux server at a
fixed size, waits for observed frames, sends keys, captures text and styles,
asks the door for independent host observations, proves the client's exit
and cleans up. Stories supply the acts and the assertions.
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
import unicodedata
from typing import Callable

ROOT = Path(__file__).resolve().parents[2]
AMUX = ROOT / "target/debug/amux"
TESTNET = ROOT / "target/debug/testnet"
MANIFEST = ROOT / "journeys/manifest.json"
OUTPUT = ROOT / "target/journeys"
GOLDENS = ROOT / "journeys/goldens/terminal"
COLS = 110
ROWS = 34


class DoorError(RuntimeError):
    pass


def door(address: str, request: object, timeout: float = 90.0) -> object:
    """One control request; the reply's value, or DoorError."""
    host, port = address.rsplit(":", 1)
    with socket.create_connection((host, int(port)), timeout=timeout) as connection:
        connection.settimeout(timeout)
        connection.sendall((json.dumps(request, separators=(",", ":")) + "\n").encode())
        with connection.makefile("rb") as stream:
            encoded = stream.readline()
    if not encoded:
        raise DoorError(f"the door closed on {request!r}")
    reply = json.loads(encoded)
    if "ok" not in reply:
        raise DoorError(f"door refused {request!r}: {reply!r}")
    return reply["ok"]


def _read_readiness(process: subprocess.Popen[bytes], timeout: float = 120.0) -> dict:
    assert process.stdout is not None
    deadline = time.monotonic() + timeout
    buffered = b""
    while time.monotonic() < deadline:
        readable, _, _ = select.select([process.stdout], [], [], 0.5)
        if not readable:
            if process.poll() is not None:
                raise RuntimeError(f"testnet exited before readiness ({process.returncode})")
            continue
        chunk = os.read(process.stdout.fileno(), 4096)
        if not chunk and process.poll() is not None:
            raise RuntimeError(f"testnet exited before readiness ({process.returncode})")
        buffered += chunk
        if b"\n" in buffered:
            line, _ = buffered.split(b"\n", 1)
            return json.loads(line)
    raise RuntimeError("testnet readiness timed out")


# --- text and styles --------------------------------------------------------

SGR = re.compile(r"\x1b\[([0-9;:]*)m")


def _cell_width(character: str) -> int:
    if unicodedata.combining(character):
        return 0
    return 2 if unicodedata.east_asian_width(character) in ("W", "F") else 1


def _apply_sgr(style: dict, params: str) -> dict:
    style = dict(style)
    codes = [int(code) if code else 0 for code in params.replace(":", ";").split(";")]
    i = 0
    while i < len(codes):
        code = codes[i]
        if code == 0:
            style = {}
        elif code in (1, 2, 3, 4, 7, 9):
            style[{1: "bold", 2: "dim", 3: "italic", 4: "underline", 7: "reverse", 9: "strike"}[code]] = True
        elif code in (22, 23, 24, 27, 29):
            for name in {22: ("bold", "dim"), 23: ("italic",), 24: ("underline",), 27: ("reverse",), 29: ("strike",)}[code]:
                style.pop(name, None)
        elif 30 <= code <= 37 or 90 <= code <= 97:
            style["fg"] = str(code)
        elif 40 <= code <= 47 or 100 <= code <= 107:
            style["bg"] = str(code)
        elif code == 39:
            style.pop("fg", None)
        elif code == 49:
            style.pop("bg", None)
        elif code in (38, 48) and i + 1 < len(codes):
            key = "fg" if code == 38 else "bg"
            if codes[i + 1] == 5 and i + 2 < len(codes):
                style[key] = f"5;{codes[i + 2]}"
                i += 2
            elif codes[i + 1] == 2 and i + 4 < len(codes):
                style[key] = "2;" + ";".join(str(c) for c in codes[i + 2 : i + 5])
                i += 4
        i += 1
    return style


def parse_styled(dump: str, cols: int) -> tuple[list[str], list[list[str]]]:
    """tmux `capture-pane -e` output as text rows and one style key per cell."""
    texts: list[str] = []
    styles: list[list[str]] = []
    style: dict = {}
    for line in dump.split("\n"):
        text = ""
        keys: list[str] = []
        at = 0
        for match in SGR.finditer(line):
            for character in line[at : match.start()]:
                text += character
                key = json.dumps(style, sort_keys=True)
                keys.extend([key] * _cell_width(character))
            style = _apply_sgr(style, match.group(1))
            at = match.end()
        for character in line[at:]:
            text += character
            keys.extend([json.dumps(style, sort_keys=True)] * _cell_width(character))
        keys.extend([json.dumps({}, sort_keys=True)] * max(0, cols - len(keys)))
        texts.append(text.rstrip())
        styles.append(keys[:cols])
    return texts, styles


def style_map(styles: list[list[str]]) -> str:
    """One character per cell: '.' for the default style, then letters in
    order of first appearance, with a legend naming each."""
    legend: dict[str, str] = {json.dumps({}, sort_keys=True): "."}
    letters = iter("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789")
    rows = []
    for row in styles:
        out = ""
        for key in row:
            if key not in legend:
                legend[key] = next(letters, "?")
            out += legend[key]
        rows.append(out)
    names = [f"{letter} = {key}" for key, letter in legend.items() if letter != "."]
    return "\n".join(rows) + "\n--- legend\n" + "\n".join(names) + "\n"


# Relative ages move with the clock; they are the only volatile field.
AGE = re.compile(r"\b(just now|\d+[smhd] ago)\b")


# The served net's scratch directories carry random suffixes of fixed
# length; masking them keeps every width.
SCRATCH = re.compile(r"(aj-|testnet)([A-Za-z0-9_]{6,8})\b")


# The fleet's age cell is compact ("now", "2m", "3h") and sits between
# two cell gaps.
FLEET_AGE = re.compile(r"(?<= )(now|\d{1,2}[mhd])(?=  )")


# A prompt's time of day sits at its row's right edge.
CLOCK = re.compile(r"(?<= )\d{2}:\d{2}$", re.M)


def normalize(text: str) -> str:
    text = CLOCK.sub("hh:mm", text)
    text = AGE.sub(lambda match: "<age>".ljust(len(match.group(0))), text)
    text = FLEET_AGE.sub(lambda match: "<a>" if len(match.group(0)) == 3 else "<>", text)
    return SCRATCH.sub(lambda match: match.group(1) + "x" * len(match.group(2)), text)


# Durations measured on the run's own clock ("380ms", "1.2s", "1m 4s") are
# volatile: each becomes "<t>". Right-aligned meta (a run of blanks before it)
# keeps its place, the blanks absorbing the difference; a duration flowing in
# a sentence after one space just takes the mask's own width, what follows it
# moving along, and the row keeps the pane's width: its last cell fills in or
# gives way at the end, so the style map does not carry the duration's width.
DURATION = re.compile(r"(?<= )(\d+m \d+s|\d+(?:\.\d+)?(?:ms|s))(?= · |$)")


def logical_paths(
    texts: list[str], styles: list[list[str]], physical: str, logical: str
) -> tuple[list[str], list[list[str]]]:
    """Spell the scratch root as the driver named it wherever a program
    printed its physical path instead. A program reading its own working
    directory gets the physical path, and on macOS /tmp is a link to
    /private/tmp: without this a frame recorded there would never match one
    from Linux. The dropped prefix's cells leave the row, as they would had
    the program printed the shorter path."""
    if physical == logical or not physical.endswith(logical[1:]):
        return texts, styles
    drop = len(physical) - len(logical)
    out_texts, out_styles = [], []
    for text, row in zip(texts, styles):
        row = list(row)
        while (start := text.find(physical)) >= 0:
            cell = sum(_cell_width(c) for c in text[:start])
            row = row[:cell] + row[cell + drop :] + [row[-1]] * drop
            text = text[:start] + text[start + drop :]
        out_texts.append(text)
        out_styles.append(row)
    return out_texts, out_styles


def mask_durations(
    texts: list[str], styles: list[list[str]]
) -> tuple[list[str], list[list[str]]]:
    out_texts, out_styles = [], []
    for text, row in zip(texts, styles):
        row = list(row)
        width = len(row)
        for match in reversed(list(DURATION.finditer(text))):
            start, end = match.span()
            blank = start
            while blank > 0 and text[blank - 1] == " ":
                blank -= 1
            extra = (end - start) - 3 if start - blank > 1 else 0
            if start - blank + extra < 1:
                continue
            # Cell positions: every character before a duration in these
            # rows is one cell wide except the glyphs parse_styled counted.
            cell = lambda index: sum(_cell_width(c) for c in text[:index])
            c_blank, c_start, c_end = cell(blank), cell(start), cell(end)
            fill = row[c_blank] if c_blank < c_start else row[c_start]
            row[c_blank:c_end] = [fill] * (c_start - c_blank + extra) + [row[c_start]] * 3
            text = text[:blank] + " " * (start - blank + extra) + "<t>" + text[end:]
        if row:
            row = (row + [row[-1]] * width)[:width]
        out_texts.append(text)
        out_styles.append(row)
    return out_texts, out_styles


# --- reading frames -------------------------------------------------------


def at_home(frame: str) -> bool:
    """Home's title line: amux at the left, the fleet's facts at the right."""
    return any(line == "  amux" or line.startswith("  amux ") for line in frame.splitlines()[:3])


def chat_of(frame: str) -> str | None:
    """The agent whose chat is open: the header's first cell."""
    for line in frame.splitlines()[:3]:
        found = re.match(r"^  (\S.*?) │ ", line)
        if found:
            return found.group(1)
    return None


def agent_row(frame: str, agent: str) -> int | None:
    """The line of home's row for `agent`: a mark, then its name."""
    pattern = re.compile(rf"^\s+(› )?\S {re.escape(agent)}(  | ▸|$)")
    return next((i for i, line in enumerate(frame.splitlines()) if pattern.match(line)), None)


def selected_row(frame: str, agent: str) -> bool:
    """Whether home's selection is on `agent`'s row: a band around it in
    full colour, a › before it in the sixteen colours."""
    at = agent_row(frame, agent)
    lines = frame.splitlines()
    return at is not None and (lines[at].lstrip().startswith("› ") or (at > 0 and "▄▄▄" in lines[at - 1]))


def working(frame: str) -> bool:
    """A turn is under way: the keys under the composer offer to stop it."""
    return "ctrl+x stop" in frame


def at_rest(frame: str, *terms: str) -> bool:
    """Every term shows and no turn is under way."""
    return all(term in frame for term in terms) and not working(frame)


@dataclass(frozen=True)
class Frame:
    label: str
    text: str
    styles: str


class TerminalJourney:
    def __init__(self, story: dict, topology: Path):
        self.story = story
        self.name = story["id"]
        self.output = OUTPUT / self.name
        if self.output.exists():
            shutil.rmtree(self.output)
        self.output.mkdir(parents=True)
        # Short: daemons bind Unix sockets under it, and those paths are
        # limited to about a hundred bytes.
        self.scratch_owner = tempfile.TemporaryDirectory(prefix="aj-", dir="/tmp")
        self.scratch = Path(self.scratch_owner.name)
        self.server = f"amux-journey-{os.getpid()}"
        self.process: subprocess.Popen[bytes] | None = None
        self.ready: dict = {}
        self.actions: list[str] = []
        self.observations: dict[str, object] = {}
        self.frames: list[Frame] = []
        env = {k: v for k, v in os.environ.items() if k not in ("AMUX_LOG", "AMUX_CONFIG")}
        env.update({key: str(self.scratch) for key in ("TMPDIR", "TMP", "TEMP")})
        self.process = subprocess.Popen(
            [str(TESTNET), "serve", str(topology)],
            cwd=ROOT,
            env=env,
            stdout=subprocess.PIPE,
            stderr=open(self.output / "testnet.log", "wb"),
        )
        try:
            self.ready = _read_readiness(self.process)
        except BaseException:
            self.process.kill()
            self.process.wait(timeout=30)
            raise
        self.actions.append(f"ready control={self.ready['control']}")

    # --- the door ---------------------------------------------------------

    def request(self, request: object, label: str | None = None) -> object:
        self.actions.append("door " + json.dumps(request, separators=(",", ":")))
        reply = door(self.ready["control"], request)
        if label is not None:
            self.observations[label] = reply
        return reply

    def chat(self, host: str, agent: str, label: str | None = None) -> dict:
        reply = self.request({"Chat": {"host": host, "agent": agent}}, label)
        assert isinstance(reply, dict)
        return reply

    def wait_chat(
        self,
        host: str,
        agent: str,
        predicate: Callable[[dict], bool],
        description: str,
        timeout: float = 60.0,
    ) -> dict:
        deadline = time.monotonic() + timeout
        last: dict = {}
        while time.monotonic() < deadline:
            last = door(self.ready["control"], {"Chat": {"host": host, "agent": agent}})
            if predicate(last):
                self.observations[description] = last
                self.actions.append(f"observed at {host}: {description}")
                return last
            time.sleep(0.2)
        raise RuntimeError(f"timed out waiting for {description}; {host} holds {last!r}")

    def inventory(self, host: str, label: str | None = None) -> list[dict]:
        reply = self.request({"Inventory": {"host": host}}, label)
        assert isinstance(reply, dict)
        return reply["agents"]

    def wait_inventory(
        self,
        host: str,
        predicate: Callable[[list[dict]], bool],
        description: str,
        timeout: float = 60.0,
    ) -> list[dict]:
        deadline = time.monotonic() + timeout
        last: list[dict] = []
        while time.monotonic() < deadline:
            last = door(self.ready["control"], {"Inventory": {"host": host}})["agents"]
            if predicate(last):
                self.observations[description] = last
                self.actions.append(f"observed at {host}: {description}")
                return last
            time.sleep(0.2)
        raise RuntimeError(f"timed out waiting for {description}; {host} lists {last!r}")

    def host_id(self, host: str) -> str:
        return next(item["host_id"] for item in self.ready["hosts"] if item["name"] == host)

    def provider_input(self, agent: str, label: str) -> list[str]:
        reply = self.request({"ProviderInput": {"agent": agent}}, label)
        assert isinstance(reply, dict)
        return reply["lines"]

    def config(self, host: str) -> str:
        return next(item["config"] for item in self.ready["hosts"] if item["name"] == host)

    # --- the terminal -----------------------------------------------------

    def tmux(self, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["tmux", "-L", self.server, *args],
            check=check,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=30,
        )

    def launch(self, pane: str, host: str, *args: str) -> str:
        """The real client against `host`'s install, in a pane of its own."""
        command = " ".join(
            [
                "env",
                "-u",
                "AMUX_LOG",
                "-u",
                "AMUX_CONFIG",
                "TERM=xterm-256color",
                # Goldens are approved in ANSI colour, which every terminal
                # the client meets can show.
                "COLORTERM=",
                shlex.quote(str(AMUX)),
                "--config",
                shlex.quote(self.config(host)),
                *[shlex.quote(arg) for arg in args],
            ]
        )
        held = command + "; status=$?; echo AMUX_EXIT_$status; sleep 600"
        # In the host's work directory, as a person starts amux in a
        # project: a new agent works where the client was started.
        work = Path(self.ready["root"]) / host / "work"
        self.tmux(
            "new-session", "-d", "-x", str(COLS), "-y", str(ROWS), "-s", pane,
            "-c", str(work if work.is_dir() else ROOT), "sh", "-c", held,
        )
        self.actions.append(f"launch {pane} on {host}: amux {' '.join(args)}")
        return pane

    def capture(self, pane: str) -> str:
        return self.tmux("capture-pane", "-p", "-t", pane).stdout

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
        return self.wait(pane, lambda frame: all(t in frame for t in terms), repr(terms), timeout)

    def keys(self, pane: str, *keys: str) -> None:
        for key in keys:
            self.tmux("send-keys", "-t", pane, key)
            # tmux sends back-to-back Escape and a letter as Alt+letter.
            time.sleep(0.15)
        self.actions.append(f"{pane}: keys {' '.join(keys)}")

    def type(self, pane: str, value: str) -> None:
        self.tmux("send-keys", "-t", pane, "-l", value)
        self.actions.append(f"{pane}: type {value!r}")

    def paste(self, pane: str, value: str) -> None:
        """A bracketed paste, as a terminal delivers one."""
        self.tmux("set-buffer", "-b", "journey", value)
        self.tmux("paste-buffer", "-p", "-d", "-b", "journey", "-t", pane)
        self.actions.append(f"{pane}: paste {len(value.splitlines())} lines")

    def select_agent(self, pane: str, agent: str) -> str:
        """Moves home's selection onto `agent`: the row inside the band."""
        self.wait(pane, lambda frame: agent_row(frame, agent) is not None, f"{agent} on home")
        self.keys(pane, "g")
        for _ in range(40):
            frame = self.capture(pane)
            if selected_row(frame, agent):
                self.actions.append(f"{pane}: selected {agent}")
                return frame
            self.keys(pane, "Down")
        raise RuntimeError(f"could not select {agent}; final frame:\n{self.capture(pane)}")

    def open_exited(self, pane: str) -> str:
        """Unfolds home's Exited section, the last thing on home while it
        is folded."""
        self.keys(pane, "G")
        self.wait_terms(pane, "▸ Exited ", "enter show")
        self.keys(pane, "Enter")
        return self.wait_terms(pane, "▾ Exited ")

    def open_chat(self, pane: str, agent: str) -> str:
        self.select_agent(pane, agent)
        self.keys(pane, "Enter")
        return self.wait(pane, lambda frame: chat_of(frame) == agent, f"{agent}'s chat")

    def home(self, pane: str) -> str:
        """Back to home from a chat."""
        if not at_home(self.capture(pane)):
            self.keys(pane, "C-a", "h")
        return self.wait(pane, at_home, "home")

    def frame(self, pane: str, label: str) -> Frame:
        """Text and styles of what the pane shows now, compared with its
        reviewed golden."""
        dump = self.tmux("capture-pane", "-p", "-e", "-t", pane).stdout
        texts, styles = parse_styled(dump.removesuffix("\n"), COLS)
        texts, styles = logical_paths(
            texts, styles, os.path.realpath(self.scratch), str(self.scratch)
        )
        texts, styles = mask_durations(texts, styles)
        text = normalize("\n".join(texts) + "\n")
        frame = Frame(label, text, style_map(styles))
        self.frames.append(frame)
        actual = self.output / "actual"
        actual.mkdir(exist_ok=True)
        (actual / f"{label}.txt").write_text(frame.text)
        (actual / f"{label}.styles").write_text(frame.styles)
        self.actions.append(f"captured {label}")
        self._compare(frame)
        return frame

    def _compare(self, frame: Frame) -> None:
        golden_dir = GOLDENS / self.name
        pairs = {
            golden_dir / f"{frame.label}.txt": frame.text,
            golden_dir / f"{frame.label}.styles": frame.styles,
        }
        update = os.environ.get("UPDATE_JOURNEY_GOLDENS") == "1"
        if update and os.environ.get("CI"):
            raise RuntimeError("UPDATE_JOURNEY_GOLDENS is refused in CI")
        for path, actual in pairs.items():
            if update:
                golden_dir.mkdir(parents=True, exist_ok=True)
                path.write_text(actual)
                continue
            if not path.exists():
                raise RuntimeError(
                    f"missing journey golden {path}; review with UPDATE_JOURNEY_GOLDENS=1"
                )
            approved = path.read_text()
            if approved != actual:
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

    def quit_client(self, pane: str) -> None:
        """Back home if a chat is open, then q; the client must say it
        exited cleanly."""
        self.home(pane)
        self.keys(pane, "q")
        self.wait_terms(pane, "AMUX_EXIT_0", timeout=30)
        self.tmux("kill-session", "-t", pane, check=False)
        self.actions.append(f"{pane}: exited 0")

    # --- the end ----------------------------------------------------------

    def finish(self, assertions: list[str]) -> None:
        self._write("PASS\n" + "".join(f"- {item}\n" for item in assertions))

    def fail(self, error: BaseException) -> None:
        self._write(f"FAIL\n- {type(error).__name__}: {error}\n")

    def _write(self, result: str) -> None:
        (self.output / "actions.txt").write_text("\n".join(self.actions) + "\n")
        (self.output / "observations.json").write_text(
            json.dumps(self.observations, indent=2, sort_keys=True) + "\n"
        )
        (self.output / "result.txt").write_text(result)

    def close(self) -> None:
        try:
            # A private tmux server: killing it touches no one else's panes.
            self.tmux("kill-server", check=False)
            if self.process is not None and self.process.poll() is None:
                try:
                    host, port = self.ready["control"].rsplit(":", 1)
                    with socket.create_connection((host, int(port)), timeout=10) as connection:
                        connection.sendall(b'"Shutdown"\n')
                        connection.recv(4096)
                except OSError:
                    pass
                try:
                    self.process.wait(timeout=60)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=30)
                    raise RuntimeError("testnet did not shut down within a minute")
            try:
                door(self.ready["control"], {"Inventory": {"host": "desk"}}, 1)
            except (OSError, DoorError):
                self.actions.append("teardown: the door is closed")
            else:
                raise RuntimeError("the door stayed open after teardown")
            (self.output / "actions.txt").write_text("\n".join(self.actions) + "\n")
        finally:
            self.scratch_owner.cleanup()


def story(name: str) -> dict:
    manifest = json.loads(MANIFEST.read_text())
    matches = [item for item in manifest["journeys"] if item["id"] == name]
    if len(matches) != 1 or "terminal" not in matches[0].get("clients", []):
        raise RuntimeError(f"no terminal journey named {name!r}")
    return matches[0]

#!/usr/bin/env python3
"""Prove a final executable carries SQLite instead of loading it dynamically."""

from dataclasses import dataclass
from pathlib import Path
import shutil
import subprocess
import sys


@dataclass(frozen=True)
class LinkageReport:
    binary: Path
    dependencies_command: tuple[str, ...]
    dependencies: str
    symbol_command: tuple[str, ...]
    symbol_lines: tuple[str, ...]

    def render(self) -> str:
        dependency_command = " ".join(self.dependencies_command)
        symbol_command = " ".join(self.symbol_command)
        symbols = "\n".join(self.symbol_lines)
        return (
            f"SQLite linkage: {self.binary}\n"
            f"$ {dependency_command}\n{self.dependencies.rstrip()}\n"
            f"$ {symbol_command} | grep sqlite3_open_v2\n{symbols}\n"
            "sqlite3_open_v2: defined\n"
            "dynamic libsqlite3: absent\n"
        )


def command_output(command: tuple[str, ...]) -> str:
    return subprocess.run(
        command, check=True, capture_output=True, text=True, timeout=120,
    ).stdout


def inspect(binary: Path) -> LinkageReport:
    binary = binary.resolve()
    if shutil.which("otool"):
        dependencies_command = ("otool", "-L", str(binary))
    elif shutil.which("ldd"):
        dependencies_command = ("ldd", str(binary))
    else:
        raise RuntimeError("neither otool nor ldd is available for linkage inspection")
    dependencies = command_output(dependencies_command)
    if "libsqlite3" in dependencies.lower():
        raise RuntimeError(f"{binary} dynamically links libsqlite3:\n{dependencies}")

    symbol_command = ("nm", "-g", str(binary))
    symbols = command_output(symbol_command)
    symbol_lines = tuple(
        line for line in symbols.splitlines()
        if line.split() and line.split()[-1].lstrip("_") == "sqlite3_open_v2"
    )
    if not symbol_lines:
        raise RuntimeError(f"{binary} does not expose a sqlite3_open_v2 symbol")
    undefined = [
        line for line in symbol_lines
        if len(line.split()) >= 2 and line.split()[-2].upper() == "U"
    ]
    if len(undefined) == len(symbol_lines):
        raise RuntimeError(
            f"{binary} imports sqlite3_open_v2 instead of defining it:\n"
            + "\n".join(symbol_lines)
        )
    return LinkageReport(
        binary, dependencies_command, dependencies, symbol_command, symbol_lines,
    )


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit(f"usage: {sys.argv[0]} EXECUTABLE")
    print(inspect(Path(sys.argv[1])).render(), end="", flush=True)


if __name__ == "__main__":
    main()

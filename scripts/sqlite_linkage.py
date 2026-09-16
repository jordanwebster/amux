#!/usr/bin/env python3
"""Prove a final executable carries SQLite instead of loading it dynamically."""

from dataclasses import dataclass
from pathlib import Path
import shutil
import struct
import subprocess
import sys


@dataclass(frozen=True)
class LinkageReport:
    binary: Path
    dependencies_command: tuple[str, ...]
    dependencies: str
    symbol_command: tuple[str, ...] | None
    symbol_lines: tuple[str, ...]

    def render(self) -> str:
        dependency_command = " ".join(self.dependencies_command)
        if self.symbol_command is None:
            return (
                f"SQLite linkage: {self.binary}\n"
                f"$ {dependency_command}\n{self.dependencies.rstrip()}\n"
                "sqlite3_open_v2: bundled (no sqlite3.dll import)\n"
                "dynamic libsqlite3: absent\n"
            )

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


def _unpack_from(fmt: str, data: bytes, offset: int, description: str) -> tuple[int, ...]:
    try:
        return struct.unpack_from(fmt, data, offset)
    except struct.error as error:
        raise RuntimeError(f"malformed PE file: missing {description}") from error


def _pe_imports(binary: Path) -> tuple[str, ...]:
    """Read DLL names from a PE import directory without platform tools."""
    data = binary.read_bytes()
    if data[:2] != b"MZ":
        raise RuntimeError(f"{binary} is not a PE executable")

    (pe_offset,) = _unpack_from("<I", data, 0x3C, "PE header offset")
    if data[pe_offset:pe_offset + 4] != b"PE\0\0":
        raise RuntimeError(f"{binary} has an invalid PE signature")

    coff_offset = pe_offset + 4
    _, section_count, _, _, _, optional_size, _ = _unpack_from(
        "<HHIIIHH", data, coff_offset, "COFF header"
    )
    optional_offset = coff_offset + 20
    (magic,) = _unpack_from("<H", data, optional_offset, "optional header")
    if magic == 0x10B:
        directory_count_offset = optional_offset + 92
        directory_offset = optional_offset + 96
    elif magic == 0x20B:
        directory_count_offset = optional_offset + 108
        directory_offset = optional_offset + 112
    else:
        raise RuntimeError(f"{binary} has an unsupported PE optional header")

    (directory_count,) = _unpack_from(
        "<I", data, directory_count_offset, "data-directory count"
    )
    if directory_count < 2:
        return ()
    import_rva, import_size = _unpack_from(
        "<II", data, directory_offset + 8, "import directory"
    )
    if import_rva == 0 or import_size == 0:
        return ()

    section_offset = optional_offset + optional_size
    sections = []
    for index in range(section_count):
        offset = section_offset + index * 40
        virtual_size, virtual_address, raw_size, raw_offset = _unpack_from(
            "<IIII", data, offset + 8, f"section {index}"
        )
        sections.append((virtual_address, max(virtual_size, raw_size), raw_offset, raw_size))

    def file_offset(rva: int, description: str) -> int:
        for virtual_address, span, raw_offset, raw_size in sections:
            if virtual_address <= rva < virtual_address + span:
                relative = rva - virtual_address
                if relative >= raw_size:
                    break
                return raw_offset + relative
        raise RuntimeError(f"malformed PE file: {description} is outside file sections")

    descriptor_offset = file_offset(import_rva, "import directory")
    descriptor_end = descriptor_offset + import_size
    imports = []
    while descriptor_offset + 20 <= len(data) and descriptor_offset < descriptor_end:
        descriptor = _unpack_from(
            "<IIIII", data, descriptor_offset, "import descriptor"
        )
        if descriptor == (0, 0, 0, 0, 0):
            break
        name_offset = file_offset(descriptor[3], "import name")
        name_end = data.find(b"\0", name_offset)
        if name_end == -1:
            raise RuntimeError("malformed PE file: unterminated import name")
        try:
            imports.append(data[name_offset:name_end].decode("ascii"))
        except UnicodeDecodeError as error:
            raise RuntimeError("malformed PE file: non-ASCII import name") from error
        descriptor_offset += 20
    return tuple(imports)


def _inspect_pe(binary: Path) -> LinkageReport:
    imports = _pe_imports(binary)
    sqlite_imports = [name for name in imports if name.lower() == "sqlite3.dll"]
    dependencies = "\n".join(imports) or "(no imported DLLs)"
    if sqlite_imports:
        raise RuntimeError(f"{binary} dynamically links sqlite3.dll:\n{dependencies}")
    return LinkageReport(
        binary,
        ("inspect", "PE", "import", "table", str(binary)),
        dependencies,
        None,
        (),
    )


def inspect(binary: Path) -> LinkageReport:
    binary = binary.resolve()
    with binary.open("rb") as executable:
        if executable.read(2) == b"MZ":
            return _inspect_pe(binary)

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

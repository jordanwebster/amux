import importlib.util
from pathlib import Path
import struct
import tempfile
from unittest import mock
import unittest


ROOT = Path(__file__).resolve().parents[2]


def load():
    spec = importlib.util.spec_from_file_location(
        "sqlite_linkage", ROOT / "scripts" / "sqlite_linkage.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


linkage = load()


def pe_executable(*imports):
    data = bytearray(0x400)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", data, 0x84, 0x8664, 1, 0, 0, 0, 0xF0, 0)
    optional = 0x98
    struct.pack_into("<H", data, optional, 0x20B)
    struct.pack_into("<I", data, optional + 108, 16)
    struct.pack_into("<II", data, optional + 120, 0x1000, (len(imports) + 1) * 20)
    section = optional + 0xF0
    data[section:section + 8] = b".rdata\0\0"
    struct.pack_into("<IIII", data, section + 8, 0x200, 0x1000, 0x200, 0x200)
    name_rva = 0x1080
    for index, name in enumerate(imports):
        struct.pack_into("<IIIII", data, 0x200 + index * 20, 0, 0, 0, name_rva, 0)
        encoded = name.encode("ascii") + b"\0"
        name_offset = 0x200 + name_rva - 0x1000
        data[name_offset:name_offset + len(encoded)] = encoded
        name_rva += len(encoded)
    return data


class LinkageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.binary = Path(self.temp.name) / "app"
        self.binary.write_bytes(b"\x7fELF")

    def test_accepts_a_defined_symbol_without_a_dynamic_sqlite_dependency(self):
        outputs = {
            "otool": "/tmp/app:\n\t/usr/lib/libSystem.B.dylib\n",
            "nm": "0000000100001234 T _sqlite3_open_v2\n",
        }
        with mock.patch.object(linkage.shutil, "which", side_effect=lambda name: f"/usr/bin/{name}"), \
                mock.patch.object(
                    linkage, "command_output", side_effect=lambda command: outputs[command[0]]
                ):
            report = linkage.inspect(self.binary)
        self.assertIn("sqlite3_open_v2: defined", report.render())
        self.assertIn("dynamic libsqlite3: absent", report.render())

    def test_refuses_a_dynamic_sqlite_dependency(self):
        with mock.patch.object(linkage.shutil, "which", return_value="/usr/bin/otool"), \
                mock.patch.object(
                    linkage,
                    "command_output",
                    return_value="/tmp/app:\n\t/usr/lib/libsqlite3.dylib\n",
                ):
            with self.assertRaisesRegex(RuntimeError, "dynamically links libsqlite3"):
                linkage.inspect(self.binary)

    def test_refuses_an_undefined_sqlite_symbol(self):
        outputs = {
            "otool": "/tmp/app:\n\t/usr/lib/libSystem.B.dylib\n",
            "nm": "                 U _sqlite3_open_v2\n",
        }
        with mock.patch.object(linkage.shutil, "which", side_effect=lambda name: f"/usr/bin/{name}"), \
                mock.patch.object(
                    linkage, "command_output", side_effect=lambda command: outputs[command[0]]
                ):
            with self.assertRaisesRegex(RuntimeError, "imports sqlite3_open_v2"):
                linkage.inspect(self.binary)

    def test_accepts_a_bundled_windows_executable_without_external_tools(self):
        with tempfile.TemporaryDirectory() as temp:
            binary = Path(temp) / "amux.exe"
            binary.write_bytes(pe_executable("KERNEL32.dll", "VCRUNTIME140.dll"))
            with mock.patch.object(
                linkage, "command_output", side_effect=AssertionError("external tool used")
            ):
                report = linkage.inspect(binary)
        self.assertIn("sqlite3_open_v2: bundled", report.render())
        self.assertIn("dynamic libsqlite3: absent", report.render())

    def test_refuses_a_windows_executable_importing_sqlite3(self):
        with tempfile.TemporaryDirectory() as temp:
            binary = Path(temp) / "amux.exe"
            binary.write_bytes(pe_executable("KERNEL32.dll", "sqlite3.dll"))
            with self.assertRaisesRegex(RuntimeError, "dynamically links sqlite3.dll"):
                linkage.inspect(binary)


if __name__ == "__main__":
    unittest.main()

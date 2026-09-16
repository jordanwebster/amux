import importlib.util
from pathlib import Path
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


class LinkageTests(unittest.TestCase):
    def test_accepts_a_defined_symbol_without_a_dynamic_sqlite_dependency(self):
        outputs = {
            "otool": "/tmp/app:\n\t/usr/lib/libSystem.B.dylib\n",
            "nm": "0000000100001234 T _sqlite3_open_v2\n",
        }
        with mock.patch.object(linkage.shutil, "which", side_effect=lambda name: f"/usr/bin/{name}"), \
                mock.patch.object(
                    linkage, "command_output", side_effect=lambda command: outputs[command[0]]
                ):
            report = linkage.inspect(Path("/tmp/app"))
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
                linkage.inspect(Path("/tmp/app"))

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
                linkage.inspect(Path("/tmp/app"))


if __name__ == "__main__":
    unittest.main()

import contextlib
import importlib.util
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "stand_ins_check", ROOT / "scripts" / "stand-ins-check.py"
)
assert SPEC is not None and SPEC.loader is not None
stand_ins = importlib.util.module_from_spec(SPEC)
# Registered before it runs: its dataclasses look their module up by name.
sys.modules[SPEC.name] = stand_ins
SPEC.loader.exec_module(stand_ins)

# Removed names, split so this file is not itself a hit.
GATE = "attaches_" "elsewhere"

# The smallest tree the end state holds on.
CLEAN = {
    "crates/tui/Cargo.toml": "[dependencies]\nsyntect = { workspace = true }\n",
    "crates/tui/src/highlight.rs": "// highlighting\n",
    "crates/tui/src/app.rs": 'const WHY: &str = "its terminal is on another machine";\n',
    "crates/tui/src/run.rs": "fn wait() { std::future::pending::<()>(); }\n",
    "crates/ui-view/src/fold.rs": "pub struct Run {\n}\n",
    "crates/agent/tests/replay/io.jsonl": '{"tool":"mcp__plugin_stripe_stripe__x","model_name":1}\n',
}


class StandInsCheckTest(unittest.TestCase):
    def setUp(self):
        self.room = tempfile.TemporaryDirectory()
        self.root = Path(self.room.name)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        for path, text in CLEAN.items():
            self.write(path, text)

    def tearDown(self):
        self.room.cleanup()

    def write(self, path: str, text: str):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def check(self) -> tuple[int, str]:
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = stand_ins.check(self.root)
        return code, out.getvalue()

    def test_the_end_state_passes(self):
        code, out = self.check()
        self.assertEqual(code, 0, out)

    def test_each_planted_stand_in_fails(self):
        for path, text in [
            ("crates/tui/src/pending.rs", "pub fn gate() -> bool { false }\n"),
            ("crates/tui/src/lib.rs", "pub(crate) mod pending;\n"),
            ("crates/tui/src/home.rs", "fn f() { crate::pending::branch(); }\n"),
            ("crates/tui/src/chat/mod.rs", "fn f() { pending::branch(); }\n"),
            ("crates/tui/src/app.rs", f"fn {GATE}() -> bool {{ false }}\n"),
            ("crates/tui/src/setup.rs", "const CODEX_" "EFFORTS: &[&str] = &[];\n"),
            ("crates/ui-view/src/settings.rs", "pub const CODEX_" "PRESETS: &[&str] = &[];\n"),
            ("crates/tui/src/words.rs", "fn model_" "name(id: &str) {}\n"),
            ("crates/ui-view/src/strip.rs", "pub struct " "Strip {}\n"),
            ("crates/ui-view/src/rows.rs", "pub struct Run" "Info {}\n"),
            ("crates/ui-view/src/composer.rs", "pub struct Outbox" "Row;\n"),
            ("apps/apple/Chat.swift", "struct Outbox" "Row {}\n"),
            ("crates/ui-view/src/composer.rs", "pub fn outbox" "_rows() {}\n"),
            ("crates/ui-view/src/plan.rs", "// plans\n"),
        ]:
            with self.subTest(path=path, text=text):
                before = (self.root / path).read_text() if (self.root / path).exists() else None
                self.write(path, (before or "") + text)
                code, out = self.check()
                self.assertEqual(code, 1, out)
                if before is None:
                    (self.root / path).unlink()
                else:
                    self.write(path, before)

    def test_the_kept_highlighter_and_notice_must_remain(self):
        for path in ["crates/tui/src/highlight.rs", "crates/tui/src/app.rs", "crates/tui/Cargo.toml"]:
            with self.subTest(path=path):
                (self.root / path).unlink()
                self.assertEqual(self.check()[0], 1)
                self.write(path, CLEAN[path])


if __name__ == "__main__":
    unittest.main()

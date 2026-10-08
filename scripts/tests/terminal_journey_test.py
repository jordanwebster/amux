"""The terminal journey driver's masks: what a compared frame hides and
what it keeps."""

from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from unittest.mock import patch  # noqa: E402

from journeys import terminal  # noqa: E402
from journeys.terminal import mask_durations  # noqa: E402


def cells(text: str, width: int, styled: str = "a") -> list[str]:
    """A row styled `styled` where it has text and `.` after it."""
    return [styled if index < len(text.rstrip()) else "." for index in range(width)]


class SpellRootTest(unittest.TestCase):
    def test_the_prefix_is_one_width_on_every_platform(self):
        self.assertEqual(len(terminal.PREFIX), len(terminal.SPELLED_PREFIX))

    @patch.object(terminal, "PREFIX", "/private/tmp")
    @patch.object(terminal, "SCRATCH_ROOT", terminal.Path("/private/tmp/aj-terminal"))
    def test_macos_paths_read_as_linux_ones_whole_or_cut_by_a_dialog(self):
        spell = terminal.spell_root
        self.assertEqual(
            spell("Claude · /private/tmp/aj-terminal/net/desk/work"),
            "Claude · /tmp/journey/aj-terminal/net/desk/work",
        )
        # Cut on the right by a dialog, and on the left.
        self.assertEqual(spell("Claude · /private╭─ Hosts"), "Claude · /tmp/jou╭─ Hosts")
        self.assertEqual(spell("│ Stop? │ate/tmp/aj-terminal/net"), "│ Stop? │journey/aj-terminal/net")
        # Too short a piece to be sure of is left alone.
        self.assertEqual(spell("a /pr b"), "a /pr b")


class MaskDurationsTest(unittest.TestCase):
    def test_a_duration_in_a_sentence_keeps_the_rows_width_whatever_its_own(self):
        rows = {}
        for duration in ("5ms", "12ms", "380ms", "1s", "1m 4s"):
            text = f"  Ran deploy --check · {duration} · allowed"
            texts, styles = mask_durations([text], [cells(text, 40)])
            self.assertEqual(texts, ["  Ran deploy --check · <t> · allowed"], duration)
            self.assertEqual(len(styles[0]), 40, duration)
            rows[duration] = styles[0]
        self.assertEqual(len({"".join(row) for row in rows.values()}), 1, rows)

    def test_a_duration_ending_a_row_keeps_the_rows_width(self):
        for duration in ("380ms", "1.2s", "9s"):
            text = f"    Worked {duration}"
            texts, styles = mask_durations([text], [cells(text, 30)])
            self.assertEqual(texts, ["    Worked <t>"], duration)
            self.assertEqual("".join(styles[0]), "a" * 14 + "." * 16, duration)

    def test_right_aligned_meta_keeps_its_place(self):
        text = "  ✔ Ran make" + " " * 10 + "380ms"
        row = ["a"] * 12 + ["."] * 10 + ["m"] * 5 + ["."] * 3
        texts, styles = mask_durations([text], [row])
        self.assertEqual(texts, ["  ✔ Ran make" + " " * 12 + "<t>"])
        self.assertEqual(styles[0], ["a"] * 12 + ["."] * 12 + ["m"] * 3 + ["."] * 3)


if __name__ == "__main__":
    unittest.main()

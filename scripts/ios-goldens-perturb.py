#!/usr/bin/env python3
"""Move one design token and require the golden run to fail.

A suite nobody has seen fail is not evidence that it would. This reaches one
whole screen the way the golden run does, with one colour token replaced
before anything is drawn, and fails unless every photograph of it came back
different with a difference image beside it. Arguments go to the golden run:
`--only ID` picks other screens, `--perturb TOKEN` another token.
"""

from pathlib import Path
import subprocess
import sys

SCREEN = "fleet"
TOKEN = "accent"


def main() -> int:
    arguments = sys.argv[1:]
    if "--only" not in arguments:
        arguments += ["--only", SCREEN]
    if "--perturb" not in arguments:
        arguments += ["--perturb", TOKEN]
    script = Path(__file__).with_name("ios-goldens.py")
    return subprocess.run([sys.executable, "-B", str(script), *arguments], timeout=1100).returncode


if __name__ == "__main__":
    sys.exit(main())

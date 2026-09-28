#!/usr/bin/env python3
"""Change the fleet on purpose and require the golden run to notice.

A suite nobody has seen fail is not evidence that it would. This reaches one
whole screen the way the golden run does, twice: once with a colour token
replaced before anything is drawn, once with only the needs-you dot taken
away (some 450 pixels no element geometry names, so only the pixel budget
can notice). Each run fails unless every photograph of the screen came back
different with a difference image beside it. Arguments go to the golden run:
`--only ID` picks other screens, `--perturb NAME` runs that one change alone.
"""

from pathlib import Path
import subprocess
import sys

SCREEN = "fleet"
# A design colour token, and the one small mark a driving build can hide.
PERTURBATIONS = ("accent", "needs-you-dot")


def main() -> int:
    arguments = sys.argv[1:]
    if "--only" not in arguments:
        arguments += ["--only", SCREEN]
    runs = [arguments] if "--perturb" in arguments else [
        [*arguments, "--perturb", perturbation] for perturbation in PERTURBATIONS
    ]
    script = Path(__file__).with_name("ios-goldens.py")
    failed = 0
    for run in runs:
        failed |= subprocess.run([sys.executable, "-B", str(script), *run], timeout=550).returncode
    return failed


if __name__ == "__main__":
    sys.exit(main())

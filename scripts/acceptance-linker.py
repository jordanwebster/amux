#!/usr/bin/env python3
"""Time linker invocations for the disposable build-acceptance workload."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import time


def main() -> int:
    real_linker = os.environ["AMUX_REAL_LINKER"]
    timing_log = Path(os.environ["AMUX_LINK_TIMING_LOG"])
    started = time.perf_counter()
    result = subprocess.run([real_linker, *sys.argv[1:]], check=False)
    elapsed = time.perf_counter() - started
    record = json.dumps({"elapsed_seconds": elapsed, "exit_code": result.returncode}) + "\n"
    descriptor = os.open(timing_log, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    try:
        os.write(descriptor, record.encode())
    finally:
        os.close(descriptor)
    return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())

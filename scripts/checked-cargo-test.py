"""Forward Cargo test output and reject successful runs that executed no tests."""

import os
import re
import subprocess
import sys


ANSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")
SUMMARY = re.compile(
    rb"^test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; "
    rb"\d+ ignored; (\d+) measured; \d+ filtered out;"
)


def informational(args):
    separator = args.index("--") if "--" in args else len(args)
    cargo_args, harness_args = args[:separator], args[separator + 1:]
    return (
        "--no-run" in cargo_args
        or any(arg in ("--help", "-h") for arg in cargo_args + harness_args)
        or "--list" in harness_args
    )


def main(args):
    command = ["cargo", "test", *args]
    if informational(args):
        os.execvp(command[0], command)

    executed = 0
    summaries = 0
    pending = b""

    def count(line):
        nonlocal executed, summaries
        match = SUMMARY.match(ANSI.sub(b"", line))
        if match:
            summaries += 1
            executed += sum(map(int, match.groups()))

    # Keep stderr inherited and forward stdout as bytes, including partial
    # lines, so compilation diagnostics and live test progress remain visible.
    process = subprocess.Popen(command, stdout=subprocess.PIPE)
    try:
        while chunk := process.stdout.read1(65536):
            sys.stdout.buffer.write(chunk)
            sys.stdout.buffer.flush()
            lines = (pending + chunk).split(b"\n")
            pending = lines.pop()
            for line in lines:
                count(line)
        count(pending)
        status = process.wait()
    finally:
        process.stdout.close()
        if process.poll() is None:
            process.kill()
            process.wait()

    if status:
        return status if status > 0 else 128 - status
    if executed:
        return 0
    if summaries:
        print(
            "test recipe: no tests executed across the selected harnesses. "
            "Check the filter; use -- --list to inspect tests or -- --ignored "
            "to run ignored tests.",
            file=sys.stderr,
        )
    else:
        print(
            "test recipe: Cargo succeeded but reported no Rust test results. "
            "Custom test harnesses need their own test recipe.",
            file=sys.stderr,
        )
    return 1


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except KeyboardInterrupt:
        sys.exit(130)

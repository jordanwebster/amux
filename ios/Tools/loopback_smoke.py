#!/usr/bin/env python3
"""Observe real relay inventory from a Swift executable on the iOS simulator."""

import importlib.util
import json
import os
from pathlib import Path
import queue
import signal
import socket
import subprocess
import sys
import tempfile
import threading

from linkage_smoke import compile_swift, run, simulator


def control(address: str, request: object) -> None:
    host, port = address.rsplit(":", 1)
    with socket.create_connection((host, int(port)), timeout=15) as connection:
        connection.sendall((json.dumps(request) + "\n").encode())
        with connection.makefile("rb") as stream:
            reply = json.loads(stream.readline())
        if "Ack" not in reply:
            raise RuntimeError(f"Runner refused {request}: {reply}")


def released(address: str) -> None:
    host, port = address.rsplit(":", 1)
    endpoint = (host, int(port))
    with socket.socket() as connection:
        connection.settimeout(2)
        if connection.connect_ex(endpoint) == 0:
            raise RuntimeError(f"Runner listener survived shutdown: {address}")
    with socket.socket() as listener:
        # Closed accepted connections may remain in TIME_WAIT. Reuse the
        # address as a server would; the failed connect above proves closure.
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind(endpoint)


def read_ready(process: subprocess.Popen) -> dict:
    lines = queue.Queue()
    threading.Thread(target=lambda: lines.put(process.stdout.readline()), daemon=True).start()
    try:
        return json.loads(lines.get(timeout=30))
    except queue.Empty as error:
        raise RuntimeError("Runner did not become ready within 30 seconds") from error


def validate_output(output: str, expected: dict[str, str]) -> None:
    lines = [line.removeprefix("daemon_names=") for line in output.splitlines() if line.startswith("daemon_names=")]
    if len(lines) != 1 or not expected:
        raise RuntimeError(f"Expected one nonempty daemon inventory: {output}")
    observed = json.loads(lines[0])
    markers = {"mobile worker stopped", "unpaired relay hosts excluded from Fleet; discovery verified through snapshot"}
    if not observed or observed != expected or not markers.issubset(output.splitlines()):
        raise RuntimeError(f"Simulator inventory or teardown mismatch: {output}")


def round_trip(executable: Path, device: str) -> str:
    with tempfile.TemporaryDirectory(prefix="amux-ios-loopback-") as temporary:
        root = Path(temporary)
        environment = os.environ | {key: str(root) for key in ("TMPDIR", "TMP", "TEMP")}
        runner = subprocess.Popen([
            "e2e-runner", "testnet", "serve", "--topology", "e2e-tests/topologies/two-hosts.json",
        ], env=environment, stdout=subprocess.PIPE, text=True)
        try:
            ready = read_ready(runner)
            expected = {daemon["host_id"]: daemon["name"] for daemon in ready["daemons"]}
            token, = [user["token"] for user in ready["users"] if user["label"] == "personal"]
            output = run("xcrun", "simctl", "spawn", device, str(executable), ready["relay"], token, *expected, timeout=60)
            validate_output(output, expected)
            control(ready["control"], "Shutdown")
            if runner.wait(timeout=15) != 0:
                raise RuntimeError("Runner failed during shutdown")
            if runner.stdout.read():
                raise RuntimeError("Runner wrote unexpected stdout after readiness")
            released(ready["relay"])
            released(ready["control"])
            if list(root.iterdir()):
                raise RuntimeError("Runner left temporary state after shutdown")
            return output + "\nRunner teardown verified: successful exit, listeners released, temporary state removed\n"
        finally:
            if runner.poll() is None:
                runner.terminate()
                try:
                    runner.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    runner.kill()
                    runner.wait(timeout=5)
            runner.stdout.close()


def main() -> None:
    output = Path("target/ios/loopback").resolve()
    output.mkdir(parents=True, exist_ok=True)
    report = output.parent / "loopback-smoke.txt"
    report.unlink(missing_ok=True)
    subprocess.run([sys.executable, "-B", str(Path(__file__).with_name("test_loopback_smoke.py"))], check=True, timeout=15)
    spec = importlib.util.spec_from_file_location("ios_rust", "scripts/ios-rust.py")
    builder = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(builder)
    builder.build("aarch64-apple-ios-sim", output, debug_tools=True)
    directory = output / "aarch64-apple-ios-sim"
    executable = output / "amux-mobile-loopback"
    compile_swift(directory, directory / "include", Path(__file__).with_name("LoopbackSmoke.swift"), executable)
    device, already_booted = simulator()
    try:
        if not already_booted:
            run("xcrun", "simctl", "boot", device)
        run("xcrun", "simctl", "bootstatus", device, "-b", timeout=180)
        text = f"amux-golden: iPhone 17 Pro, iOS 26.5 ({device})\n" + round_trip(executable, device)
    finally:
        if not already_booted:
            run("xcrun", "simctl", "shutdown", device)
    report.write_text(text)
    print(text, end="", flush=True)


if __name__ == "__main__":
    # Allow the recipe's timeout to unwind the runner and simulator ownership.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        main()
    except subprocess.CalledProcessError as error:
        if error.stdout:
            print(error.stdout, file=sys.stderr)
        if error.stderr:
            print(error.stderr, file=sys.stderr)
        raise

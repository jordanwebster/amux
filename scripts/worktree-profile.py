#!/usr/bin/env python3
"""Create a worktree's unbound profile without replacing existing device state."""

import argparse
import fcntl
import json
import os
from pathlib import Path
import uuid


def write_new(path, value):
    with path.open("x") as output:
        os.chmod(path, 0o600)
        json.dump(value, output, indent=2)
        output.write("\n")


def generate(root, name):
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    root = root.resolve()
    profile_id = str(uuid.uuid5(uuid.NAMESPACE_URL, "amux-worktree:" + name))
    directory = root / "profiles" / profile_id
    config_path = directory / "config.yaml"
    alias = root / "profile.yaml"
    with (root / "lock").open("a+") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        os.chmod(root, 0o700)
        for path in [directory, directory / "state", directory / "data"]:
            path.mkdir(parents=True, exist_ok=True, mode=0o700)
        if not (root / "registry.yaml").exists():
            write_new(root / "registry.yaml", {"profiles": [{
                "id": profile_id,
                "label": {"account_name": None, "email": None, "override_name": name},
                "binding": None, "paused": False, "revision": 1,
            }]})
        if not config_path.exists():
            write_new(config_path, {
                "installation_config": str(root / "installation.yaml"),
                "socket_path": str(root / "profiles" / (profile_id + ".sock")),
                "state_path": str(directory / "state" / "state.yaml"),
                "data_dir": str(directory / "data"),
            })
        if not alias.is_symlink() and not alias.exists():
            alias.symlink_to(config_path.relative_to(root))
        if alias.resolve() != config_path:
            raise ValueError(f"{alias} does not select the worktree profile {profile_id}")
    return config_path


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("name")
    args = parser.parse_args()
    print(generate(args.root, args.name))

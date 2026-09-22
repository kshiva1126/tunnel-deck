#!/usr/bin/env python3
"""Generate deterministic third-party license notices for release targets."""

import argparse
import hashlib
import json
import subprocess
from collections import defaultdict
from pathlib import Path


TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
)
NOTICE_PREFIXES = ("LICENSE", "COPYING", "NOTICE", "UNLICENSE")


def metadata(target: str) -> dict:
    result = subprocess.run(
        [
            "cargo",
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            target,
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def release_packages() -> dict[tuple[str, str, str], dict]:
    packages = {}
    for target in TARGETS:
        document = metadata(target)
        reachable = {node["id"] for node in document["resolve"]["nodes"]}
        for package in document["packages"]:
            if package["id"] not in reachable or package["source"] is None:
                continue
            key = (package["name"], package["version"], package["source"])
            packages[key] = package
    return packages


def notice_files(package: dict) -> list[Path]:
    directory = Path(package["manifest_path"]).parent
    return sorted(
        path
        for path in directory.iterdir()
        if path.is_file() and path.name.upper().startswith(NOTICE_PREFIXES)
    )


def render() -> str:
    packages = release_packages()
    texts: dict[str, bytes] = {}
    users: dict[str, list[str]] = defaultdict(list)
    inventory = []
    for key, package in sorted(packages.items()):
        files = notice_files(package)
        if not package.get("license"):
            raise SystemExit(f"dependency has no declared license: {key[0]} {key[1]}")
        if not files:
            raise SystemExit(f"dependency has no packaged license/notice file: {key[0]} {key[1]}")
        labels = []
        for path in files:
            content = path.read_bytes().replace(b"\r\n", b"\n").rstrip() + b"\n"
            digest = hashlib.sha256(content).hexdigest()
            texts[digest] = content
            users[digest].append(f"{key[0]} {key[1]} ({path.name})")
            labels.append(path.name)
        inventory.append(f"- {key[0]} {key[1]}: {package['license']} [{', '.join(labels)}]")

    output = [
        "TunnelDeck third-party license notices",
        "======================================",
        "",
        "This file is generated from Cargo.lock for TunnelDeck's four release targets.",
        "It includes every license/notice file shipped by each reachable Rust package.",
        "A package may offer multiple license choices; inclusion does not alter its terms.",
        "",
        "Dependency inventory",
        "--------------------",
        *inventory,
        "",
        "License and notice texts",
        "------------------------",
    ]
    for index, digest in enumerate(sorted(texts), 1):
        output.extend(
            [
                "",
                f"Notice {index} (SHA-256 {digest})",
                "Used by: " + "; ".join(sorted(users[digest])),
                "",
                texts[digest].decode("utf-8"),
            ]
        )
    return "\n".join(output).rstrip() + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=Path("THIRD_PARTY_LICENSES.txt"))
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    generated = render()
    if args.check:
        if not args.output.is_file() or args.output.read_text(encoding="utf-8") != generated:
            raise SystemExit(f"{args.output} is missing or stale; regenerate it")
    else:
        args.output.write_text(generated, encoding="utf-8")


if __name__ == "__main__":
    main()

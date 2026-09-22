#!/usr/bin/env python3
"""Validate and aggregate the four per-target release outputs."""

import argparse
import hashlib
import json
import shutil
import tarfile
from pathlib import Path

TARGETS = (
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
)
TARGET_RUNNERS = {
    "x86_64-unknown-linux-gnu": {"os": "Linux", "architecture": "x86_64"},
    "aarch64-unknown-linux-gnu": {"os": "Linux", "architecture": "aarch64"},
    "x86_64-apple-darwin": {"os": "macOS", "architecture": "x86_64"},
    "aarch64-apple-darwin": {"os": "macOS", "architecture": "aarch64"},
}
SMOKE_CHECKS = ("version", "help", "completion", "manpage")


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def checksum(path: Path, archive_name: str) -> str:
    parts = path.read_text(encoding="ascii").strip().split("  ")
    if len(parts) != 2 or parts[1] != archive_name:
        raise SystemExit(f"invalid checksum sidecar for {archive_name}")
    return parts[0]


def archived_metadata(path: Path, stem: str) -> dict:
    with tarfile.open(path, "r:gz") as archive:
        member = archive.getmember(f"{stem}/release.json")
        source = archive.extractfile(member)
        if source is None:
            raise SystemExit(f"release metadata missing from {path.name}")
        return json.load(source)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    records = []
    checksum_lines = []
    versions = set()
    revisions = set()
    for target in TARGETS:
        matches = list(args.input.glob(f"**/tunnel-deck-*-{target}.json"))
        if len(matches) != 1:
            raise SystemExit(f"expected one metadata file for {target}, found {len(matches)}")
        record = json.loads(matches[0].read_text(encoding="utf-8"))
        archive = matches[0].with_name(record["archive"])
        checksum_path = matches[0].with_suffix(".sha256")
        expected_name = f"tunnel-deck-{record['version']}-{target}.tar.gz"
        expected_digest = checksum(checksum_path, archive.name)
        embedded_record = archived_metadata(archive, expected_name.removesuffix(".tar.gz"))
        smoke = record.get("native_smoke_test", {})
        checks = smoke.get("checks", [])
        if (
            record.get("schema_version") != 1
            or record["target"] != target
            or record["archive"] != expected_name
            or embedded_record != record
            or digest(archive) != expected_digest
            or record.get("build", {}).get("status") != "passed"
            or smoke.get("status") != "passed"
            or smoke.get("target") != target
            or smoke.get("runner") != TARGET_RUNNERS[target]
            or smoke.get("source_revision") != record["build"].get("source_revision")
            or tuple(check.get("name") for check in checks) != SMOKE_CHECKS
            or any(check.get("status") != "passed" for check in checks)
        ):
            raise SystemExit(f"invalid release evidence for {target}")
        shutil.copy2(archive, args.output / archive.name)
        record["sha256"] = expected_digest
        records.append(record)
        versions.add(record["version"])
        revisions.add(record["build"]["source_revision"])
        checksum_lines.append(f"{expected_digest}  {archive.name}")
    if len(versions) != 1 or len(revisions) != 1:
        raise SystemExit("release artifacts do not share one version and source revision")
    (args.output / "SHA256SUMS").write_text("\n".join(checksum_lines) + "\n", encoding="ascii")
    (args.output / "release-manifest.json").write_text(
        json.dumps({"schema_version": 1, "artifacts": records}, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()

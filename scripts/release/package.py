#!/usr/bin/env python3
"""Create one deterministic TunnelDeck release archive and its evidence."""

import argparse
import gzip
import hashlib
import json
import re
import subprocess
import tarfile
import tempfile
from pathlib import Path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def verify_linkage(binary: Path, target: str, minimum_os: str) -> str:
    if target.endswith("unknown-linux-gnu"):
        if minimum_os != "Linux 5.15 / glibc 2.35":
            raise SystemExit(f"unexpected Linux baseline: {minimum_os}")
        result = subprocess.run(["file", str(binary)], check=True, capture_output=True, text=True)
        if "dynamically linked" not in result.stdout:
            raise SystemExit("Linux artifact is not dynamically linked")
        versions = subprocess.run(
            ["readelf", "--version-info", str(binary)], check=True, capture_output=True, text=True
        ).stdout
        glibc_versions = [tuple(map(int, match)) for match in re.findall(r"GLIBC_(\d+)\.(\d+)", versions)]
        if not glibc_versions or max(glibc_versions) > (2, 35):
            raise SystemExit("Linux artifact exceeds or cannot prove the glibc 2.35 baseline")
        maximum = ".".join(map(str, max(glibc_versions)))
        return f"file/readelf: dynamic glibc ELF; maximum required GLIBC symbol version {maximum}"
    if target.endswith("apple-darwin"):
        if minimum_os != "macOS 13.0":
            raise SystemExit(f"unexpected macOS baseline: {minimum_os}")
        result = subprocess.run(["otool", "-L", str(binary)], check=True, capture_output=True, text=True)
        if "/usr/lib/libSystem.B.dylib" not in result.stdout:
            raise SystemExit("macOS artifact does not link libSystem dynamically")
        load_commands = subprocess.run(
            ["otool", "-l", str(binary)], check=True, capture_output=True, text=True
        ).stdout
        minimums = re.findall(r"^\s+minos\s+(\d+)\.(\d+)", load_commands, re.MULTILINE)
        if not minimums or max(tuple(map(int, value)) for value in minimums) > (13, 0):
            raise SystemExit("macOS artifact exceeds or cannot prove the macOS 13.0 deployment target")
        return "otool: dynamically linked to libSystem with LC_BUILD_VERSION minos 13.0 or earlier"
    raise SystemExit(f"unsupported target: {target}")


def add_file(archive: tarfile.TarFile, source: Path, name: str, mode: int, epoch: int) -> None:
    info = tarfile.TarInfo(name)
    info.size = source.stat().st_size
    info.mode = mode
    info.mtime = epoch
    info.uid = info.gid = 0
    info.uname = info.gname = "root"
    with source.open("rb") as stream:
        archive.addfile(info, stream)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--minimum-os", required=True)
    parser.add_argument("--linkage", required=True)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--source-date-epoch", required=True, type=int)
    args = parser.parse_args()

    if not args.binary.is_file():
        raise SystemExit(f"binary not found: {args.binary}")
    link_evidence = verify_linkage(args.binary, args.target, args.minimum_os)
    smoke = {
        "status": "not_run",
        "reason": "native testing of the packaged distribution artifact is tracked separately",
    }

    stem = f"tunnel-deck-{args.version}-{args.target}"
    args.output.mkdir(parents=True, exist_ok=True)
    archive_path = args.output / f"{stem}.tar.gz"
    metadata_path = args.output / f"{stem}.json"
    checksum_path = args.output / f"{stem}.sha256"
    metadata = {
        "schema_version": 1,
        "product": "TunnelDeck",
        "version": args.version,
        "target": args.target,
        "minimum_os": args.minimum_os,
        "linkage": args.linkage,
        "linkage_evidence": link_evidence,
        "build": {"status": "passed", "rust": "1.85.0", "source_revision": args.source_revision},
        "native_smoke_test": smoke,
    }

    with tempfile.TemporaryDirectory() as temporary:
        staged = Path(temporary) / "release.json"
        staged.write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        with archive_path.open("wb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=args.source_date_epoch) as zipped:
                with tarfile.open(fileobj=zipped, mode="w", format=tarfile.USTAR_FORMAT) as archive:
                    add_file(archive, Path("LICENSE"), f"{stem}/LICENSE", 0o644, args.source_date_epoch)
                    add_file(archive, staged, f"{stem}/release.json", 0o644, args.source_date_epoch)
                    add_file(archive, args.binary, f"{stem}/tdeck", 0o755, args.source_date_epoch)

    metadata["archive"] = archive_path.name
    metadata["sha256"] = sha256(archive_path)
    metadata_path.write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    checksum_path.write_text(f"{metadata['sha256']}  {archive_path.name}\n", encoding="ascii")


if __name__ == "__main__":
    main()

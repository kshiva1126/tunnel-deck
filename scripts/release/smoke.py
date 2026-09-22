#!/usr/bin/env python3
"""Natively smoke-test the exact TunnelDeck archive selected for upload."""

import argparse
import json
import platform
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

if __package__:
    from scripts.release import package
else:
    sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
    from scripts.release import package


TARGET_RUNNERS = {
    "x86_64-unknown-linux-gnu": ("Linux", "x86_64"),
    "aarch64-unknown-linux-gnu": ("Linux", "aarch64"),
    "x86_64-apple-darwin": ("macOS", "x86_64"),
    "aarch64-apple-darwin": ("macOS", "aarch64"),
}

CHECKS = (
    ("version", ("--version",)),
    ("help", ("--help",)),
    ("completion", ("completion", "bash")),
    ("manpage", ("manpage",)),
)
COMMAND_TIMEOUT_SECONDS = 30


def runner_identity() -> tuple[str, str]:
    system = {"Darwin": "macOS"}.get(platform.system(), platform.system())
    machine = {"arm64": "aarch64", "AMD64": "x86_64"}.get(
        platform.machine(), platform.machine()
    )
    return system, machine


def extract_archive(
    archive_path: Path, destination: Path, stem: str
) -> tuple[Path, Path, Path, dict, int]:
    expected = [
        f"{stem}/LICENSE",
        f"{stem}/THIRD_PARTY_LICENSES.txt",
        f"{stem}/release.json",
        f"{stem}/tdeck",
    ]
    with tarfile.open(archive_path, "r:gz") as archive:
        members = archive.getmembers()
        if [member.name for member in members] != expected or any(not member.isfile() for member in members):
            raise RuntimeError("archive layout does not match the release contract")
        extracted = destination / stem
        extracted.mkdir(parents=True)
        for member in members:
            source = archive.extractfile(member)
            if source is None:
                raise RuntimeError(f"archive member cannot be read: {member.name}")
            target = extracted / Path(member.name).name
            target.write_bytes(source.read())
            target.chmod(member.mode)
        epoch = members[0].mtime
    metadata = json.loads((extracted / "release.json").read_text(encoding="utf-8"))
    return (
        extracted / "tdeck",
        extracted / "LICENSE",
        extracted / "THIRD_PARTY_LICENSES.txt",
        metadata,
        epoch,
    )


def output_is_valid(name: str, output: str, version: str) -> bool:
    if name == "version":
        return output.strip() == f"tdeck {version}"
    if name == "help":
        return "Usage: tdeck" in output and "Commands:" in output
    if name == "completion":
        return "_tdeck()" in output and "complete -F _tdeck" in output
    if name == "manpage":
        return ".TH tdeck 1" in output and ".SH NAME" in output
    return False


def run_checks(binary: Path, version: str) -> list[dict]:
    results = []
    for name, arguments in CHECKS:
        completed = subprocess.run(
            [str(binary), *arguments],
            check=True,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=COMMAND_TIMEOUT_SECONDS,
        )
        if not output_is_valid(name, completed.stdout, version):
            raise RuntimeError(f"{name} produced unexpected output")
        results.append({"name": name, "arguments": list(arguments), "status": "passed"})
    return results


def smoke(metadata_path: Path) -> None:
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    target = metadata.get("target")
    archive_name = metadata.get("archive")
    checksum_path = metadata_path.with_suffix(".sha256")
    cleanup_paths = [metadata_path, checksum_path]
    try:
        if (
            not isinstance(archive_name, str)
            or not archive_name
            or Path(archive_name).name != archive_name
        ):
            raise RuntimeError("metadata does not contain a safe archive name")
        archive_path = metadata_path.with_name(archive_name)
        cleanup_paths.append(archive_path)
        stem = archive_path.name.removesuffix(".tar.gz")
        if target not in TARGET_RUNNERS:
            raise RuntimeError(f"unsupported smoke target: {target}")
        actual_runner = runner_identity()
        if actual_runner != TARGET_RUNNERS[target]:
            raise RuntimeError(
                f"target {target} requires runner {TARGET_RUNNERS[target]}, got {actual_runner}"
            )
        if metadata.get("native_smoke_test", {}).get("status") != "not_run":
            raise RuntimeError("input archive must have unverified smoke status")
        checksum_parts = checksum_path.read_text(encoding="ascii").strip().split("  ")
        if (
            len(checksum_parts) != 2
            or checksum_parts[1] != archive_path.name
            or checksum_parts[0] != package.sha256(archive_path)
        ):
            raise RuntimeError("candidate archive checksum does not match its sidecar")

        with tempfile.TemporaryDirectory() as first_directory:
            binary, license_path, notices_path, archived_metadata, epoch = extract_archive(
                archive_path, Path(first_directory), stem
            )
            if archived_metadata != metadata:
                raise RuntimeError("archive and sidecar metadata differ before smoke")
            checks = run_checks(binary, metadata["version"])
            metadata["native_smoke_test"] = {
                "status": "passed",
                "target": target,
                "runner": {"os": actual_runner[0], "architecture": actual_runner[1]},
                "source_revision": metadata["build"]["source_revision"],
                "checks": checks,
            }
            package.write_archive(
                archive_path,
                stem,
                binary,
                license_path,
                notices_path,
                metadata,
                epoch,
            )
            package.write_sidecars(archive_path, metadata_path, checksum_path, metadata)

        # Re-run from a fresh extraction after evidence changed the archive bytes.
        with tempfile.TemporaryDirectory() as final_directory:
            binary, _, _, archived_metadata, _ = extract_archive(
                archive_path, Path(final_directory), stem
            )
            if archived_metadata != metadata:
                raise RuntimeError("final archive and sidecar metadata differ")
            run_checks(binary, metadata["version"])
    except BaseException:
        for path in cleanup_paths:
            try:
                path.unlink()
            except FileNotFoundError:
                pass
        raise


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--metadata", type=Path, required=True)
    args = parser.parse_args()
    smoke(args.metadata)


if __name__ == "__main__":
    main()

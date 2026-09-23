import hashlib
import json
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts.release import aggregate, package, smoke


class ReleasePackagingTest(unittest.TestCase):
    def test_archive_is_deterministic_and_metadata_matches_sidecar(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "tdeck"
            binary.write_bytes(b"binary")
            output = root / "out"
            arguments = [
                "package.py", "--target", "aarch64-unknown-linux-gnu", "--binary", str(binary),
                "--output", str(output), "--version", "0.1.0", "--minimum-os", "Linux 5.15 / glibc 2.35",
                "--linkage", "dynamic glibc", "--source-revision", "abc123", "--source-date-epoch", "1",
            ]
            completed = [
                mock.Mock(returncode=0, stdout="ELF dynamically linked"),
                mock.Mock(returncode=0, stdout="Name: GLIBC_2.34"),
            ]
            with mock.patch("sys.argv", arguments), mock.patch.object(package.subprocess, "run", side_effect=completed):
                package.main()
            archive = next(output.glob("*.tar.gz"))
            first = hashlib.sha256(archive.read_bytes()).hexdigest()
            with mock.patch("sys.argv", arguments), mock.patch.object(package.subprocess, "run", side_effect=completed):
                package.main()
            self.assertEqual(first, hashlib.sha256(archive.read_bytes()).hexdigest())
            metadata = json.loads(next(output.glob("*.json")).read_text())
            self.assertEqual("passed", metadata["build"]["status"])
            self.assertEqual("not_run", metadata["native_smoke_test"]["status"])
            with tarfile.open(archive, "r:gz") as contents:
                names = contents.getnames()
                self.assertEqual(sorted(names), names)
                self.assertTrue(any(name.endswith("/LICENSE") for name in names))
                self.assertTrue(any(name.endswith("/THIRD_PARTY_LICENSES.txt") for name in names))
                self.assertTrue(any(name.endswith("release.json") for name in names))
                self.assertEqual({1}, {member.mtime for member in contents.getmembers()})
                archived = json.load(contents.extractfile(next(name for name in names if name.endswith("release.json"))))
            self.assertEqual(metadata, archived)

    def test_native_smoke_records_all_checks_and_retests_final_archive(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "tdeck"
            binary.write_text(
                "#!/bin/sh\n"
                "case \"$1\" in\n"
                "  --version) printf 'tdeck 0.1.0\\n' ;;\n"
                "  --help) printf 'Usage: tdeck\\nCommands:\\n' ;;\n"
                "  completion) printf '_tdeck() {}\\ncomplete -F _tdeck tdeck\\n' ;;\n"
                "  manpage) printf '.TH tdeck 1\\n.SH NAME\\n' ;;\n"
                "esac\n"
            )
            binary.chmod(0o755)
            output = root / "out"
            arguments = [
                "package.py", "--target", "x86_64-unknown-linux-gnu", "--binary", str(binary),
                "--output", str(output), "--version", "0.1.0", "--minimum-os", "Linux 5.15 / glibc 2.35",
                "--linkage", "dynamic glibc", "--source-revision", "abc123", "--source-date-epoch", "1",
            ]
            completed = [
                mock.Mock(returncode=0, stdout="ELF dynamically linked"),
                mock.Mock(returncode=0, stdout="Name: GLIBC_2.34"),
            ]
            with mock.patch("sys.argv", arguments), mock.patch.object(package.subprocess, "run", side_effect=completed):
                package.main()
            metadata_path = next(output.glob("*.json"))
            with mock.patch.object(smoke, "runner_identity", return_value=("Linux", "x86_64")), mock.patch.object(
                smoke, "run_checks", wraps=smoke.run_checks
            ) as run_checks:
                smoke.smoke(metadata_path)
            self.assertEqual(2, run_checks.call_count)
            metadata = json.loads(metadata_path.read_text())
            evidence = metadata["native_smoke_test"]
            self.assertEqual("passed", evidence["status"])
            self.assertEqual("abc123", evidence["source_revision"])
            self.assertEqual(["version", "help", "completion", "manpage"], [item["name"] for item in evidence["checks"]])
            archive = output / metadata["archive"]
            with tarfile.open(archive, "r:gz") as contents:
                member = next(item for item in contents.getmembers() if item.name.endswith("release.json"))
                self.assertEqual(metadata, json.load(contents.extractfile(member)))

    def test_native_smoke_failure_removes_publishable_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "tdeck"
            license_path = root / "LICENSE"
            notices_path = root / "THIRD_PARTY_LICENSES.txt"
            binary.write_text("#!/bin/sh\nprintf 'output\\n'\n")
            binary.chmod(0o755)
            license_path.write_text("license")
            notices_path.write_text("notices")
            stem = "tunnel-deck-0.1.0-x86_64-unknown-linux-gnu"
            metadata_path = root / f"{stem}.json"
            archive = root / f"{stem}.tar.gz"
            checksum = root / f"{stem}.sha256"
            metadata = {
                "version": "0.1.0", "target": "x86_64-unknown-linux-gnu", "archive": archive.name,
                "native_smoke_test": {"status": "not_run"}, "build": {"source_revision": "abc123"},
            }
            package.write_archive(
                archive, stem, binary, license_path, notices_path, metadata, 1
            )
            package.write_sidecars(archive, metadata_path, checksum, metadata)
            with mock.patch.object(smoke, "runner_identity", return_value=("Linux", "x86_64")), mock.patch.object(
                smoke, "run_checks", side_effect=subprocess.CalledProcessError(1, "tdeck --version")
            ):
                with self.assertRaises(subprocess.CalledProcessError):
                    smoke.smoke(metadata_path)
            self.assertFalse(metadata_path.exists())
            self.assertFalse(archive.exists())
            self.assertFalse(checksum.exists())

    def test_native_smoke_rejects_bad_candidate_checksum_and_cleans_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "tdeck"
            license_path = root / "LICENSE"
            notices_path = root / "THIRD_PARTY_LICENSES.txt"
            binary.write_text("#!/bin/sh\nexit 0\n")
            binary.chmod(0o755)
            license_path.write_text("license")
            notices_path.write_text("notices")
            stem = "tunnel-deck-0.1.0-x86_64-unknown-linux-gnu"
            metadata_path = root / f"{stem}.json"
            archive = root / f"{stem}.tar.gz"
            checksum = root / f"{stem}.sha256"
            metadata = {
                "version": "0.1.0",
                "target": "x86_64-unknown-linux-gnu",
                "archive": archive.name,
                "native_smoke_test": {"status": "not_run"},
                "build": {"source_revision": "abc123"},
            }
            package.write_archive(
                archive, stem, binary, license_path, notices_path, metadata, 1
            )
            package.write_sidecars(archive, metadata_path, checksum, metadata)
            checksum.write_text(f"{'0' * 64}  {archive.name}\n")
            with mock.patch.object(
                smoke, "runner_identity", return_value=("Linux", "x86_64")
            ):
                with self.assertRaisesRegex(RuntimeError, "checksum"):
                    smoke.smoke(metadata_path)
            self.assertFalse(metadata_path.exists())
            self.assertFalse(archive.exists())
            self.assertFalse(checksum.exists())

    def test_run_checks_requires_recognizable_output_and_sets_timeout(self):
        completed = mock.Mock(returncode=0, stdout="placeholder\n", stderr="")
        with mock.patch.object(smoke.subprocess, "run", return_value=completed) as run:
            with self.assertRaisesRegex(RuntimeError, "unexpected output"):
                smoke.run_checks(Path("tdeck"), "0.1.0")
        self.assertEqual(smoke.COMMAND_TIMEOUT_SECONDS, run.call_args.kwargs["timeout"])

    def test_native_smoke_malformed_archive_name_cleans_sidecars(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            metadata_path = root / "release.json"
            checksum_path = root / "release.sha256"
            metadata_path.write_text(json.dumps({"archive": ""}))
            checksum_path.write_text("invalid checksum\n")
            with self.assertRaisesRegex(RuntimeError, "safe archive name"):
                smoke.smoke(metadata_path)
            self.assertFalse(metadata_path.exists())
            self.assertFalse(checksum_path.exists())

    def test_third_party_notices_match_locked_release_dependencies(self):
        from scripts.licenses import generate

        expected = Path("THIRD_PARTY_LICENSES.txt").read_text(encoding="utf-8")
        self.assertEqual(expected, generate.render())
        self.assertIn("unicode-ident", expected)
        self.assertIn("Unicode-3.0", expected)
        self.assertIn("foldhash", expected)
        self.assertIn("Zlib", expected)

    def test_aggregate_requires_every_target_and_rechecks_checksums(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for target in aggregate.TARGETS:
                target_dir = root / target
                target_dir.mkdir()
                stem = f"tunnel-deck-0.1.0-{target}"
                archive = target_dir / f"{stem}.tar.gz"
                record = {
                    "schema_version": 1,
                    "version": "0.1.0",
                    "target": target,
                    "archive": archive.name,
                    "build": {"status": "passed", "source_revision": "abc123"},
                    "native_smoke_test": {
                        "status": "passed", "target": target, "source_revision": "abc123",
                        "runner": aggregate.TARGET_RUNNERS[target],
                        "checks": [
                            {"name": name, "status": "passed"} for name in aggregate.SMOKE_CHECKS
                        ],
                    },
                }
                binary = target_dir / "tdeck"
                license_path = target_dir / "LICENSE"
                notices_path = target_dir / "THIRD_PARTY_LICENSES.txt"
                binary.write_bytes(b"binary")
                license_path.write_bytes(b"license")
                notices_path.write_bytes(b"notices")
                package.write_archive(
                    archive, stem, binary, license_path, notices_path, record, 1
                )
                archive.with_suffix("").with_suffix(".json").write_text(json.dumps(record))
                archive.with_suffix("").with_suffix(".sha256").write_text(
                    f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n"
                )
            output = root / "release"
            with mock.patch("sys.argv", ["aggregate.py", "--input", str(root), "--output", str(output)]):
                aggregate.main()
            self.assertEqual(4, len((output / "SHA256SUMS").read_text().splitlines()))
            self.assertEqual(4, len(json.loads((output / "release-manifest.json").read_text())["artifacts"]))

    def test_aggregate_rejects_any_target_without_successful_smoke(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for target in aggregate.TARGETS:
                target_dir = root / target
                target_dir.mkdir()
                stem = f"tunnel-deck-0.1.0-{target}"
                archive = target_dir / f"{stem}.tar.gz"
                record = {
                    "schema_version": 1, "version": "0.1.0", "target": target,
                    "archive": archive.name, "build": {"status": "passed", "source_revision": "abc123"},
                    "native_smoke_test": {"status": "not_run"},
                }
                binary = target_dir / "tdeck"
                license_path = target_dir / "LICENSE"
                notices_path = target_dir / "THIRD_PARTY_LICENSES.txt"
                binary.write_bytes(b"binary")
                license_path.write_bytes(b"license")
                notices_path.write_bytes(b"notices")
                package.write_archive(
                    archive, stem, binary, license_path, notices_path, record, 1
                )
                archive.with_suffix("").with_suffix(".json").write_text(json.dumps(record))
                archive.with_suffix("").with_suffix(".sha256").write_text(
                    f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n"
                )
            with mock.patch("sys.argv", ["aggregate.py", "--input", str(root), "--output", str(root / "release")]):
                with self.assertRaises(SystemExit):
                    aggregate.main()


class ReleaseWorkflowTest(unittest.TestCase):
    def test_ci_smoke_tests_cargo_package_and_isolated_install_on_both_platforms(self):
        workflow = Path(".github/workflows/ci.yml").read_text()
        self.assertIn("os: [ubuntu-latest, macos-latest]", workflow)
        self.assertIn('package_files="$(cargo package --list)"', workflow)
        self.assertIn("README.md LICENSE Cargo.toml src/lib.rs src/main.rs", workflow)
        self.assertIn('cargo install --path . --locked --root "$install_root"', workflow)
        self.assertIn('"$install_root/bin/tdeck" --version', workflow)
        self.assertIn('"$install_root/bin/tdeck" --help', workflow)

    def test_workflow_is_pr_testable_and_actions_are_commit_pinned(self):
        workflow = Path(".github/workflows/release.yml").read_text()
        self.assertIn("pull_request:", workflow)
        self.assertIn("workflow_dispatch:", workflow)
        self.assertIn('- "v*"', workflow)
        for target, runner in (
            ("x86_64-unknown-linux-gnu", "ubuntu-22.04"),
            ("aarch64-unknown-linux-gnu", "ubuntu-22.04-arm"),
            ("x86_64-apple-darwin", "macos-15-intel"),
            ("aarch64-apple-darwin", "macos-15"),
        ):
            self.assertIn(f"target: {target}", workflow)
            self.assertIn(f"runner: {runner}", workflow)
        self.assertEqual(4, workflow.count("target: "))
        self.assertLess(workflow.index("scripts/release/smoke.py"), workflow.index("name: release-${{ matrix.target }}"))
        self.assertIn("needs: build", workflow)
        self.assertIn("needs: manifest", workflow)
        for workflow_path in Path(".github/workflows").glob("*.yml"):
            for line in (
                line.strip()
                for line in workflow_path.read_text().splitlines()
                if "uses: actions/" in line
            ):
                reference = line.split("@", 1)[1].split()[0]
                self.assertRegex(reference, r"^[0-9a-f]{40}$", workflow_path)


if __name__ == "__main__":
    unittest.main()

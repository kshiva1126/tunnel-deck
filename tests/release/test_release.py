import hashlib
import json
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts.release import aggregate, package


class ReleasePackagingTest(unittest.TestCase):
    def test_archive_is_deterministic_and_records_unverified_smoke_separately(self):
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
                archive = target_dir / f"tunnel-deck-0.1.0-{target}.tar.gz"
                archive.write_bytes(target.encode())
                record = {
                    "schema_version": 1,
                    "version": "0.1.0",
                    "target": target,
                    "archive": archive.name,
                    "sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
                    "build": {"status": "passed", "source_revision": "abc123"},
                    "native_smoke_test": {"status": "not_run"},
                }
                archive.with_suffix("").with_suffix(".json").write_text(json.dumps(record))
            output = root / "release"
            with mock.patch("sys.argv", ["aggregate.py", "--input", str(root), "--output", str(output)]):
                aggregate.main()
            self.assertEqual(4, len((output / "SHA256SUMS").read_text().splitlines()))
            self.assertEqual(4, len(json.loads((output / "release-manifest.json").read_text())["artifacts"]))


class ReleaseWorkflowTest(unittest.TestCase):
    def test_workflow_is_pr_testable_and_actions_are_commit_pinned(self):
        workflow = Path(".github/workflows/release.yml").read_text()
        self.assertIn("pull_request:", workflow)
        self.assertIn("workflow_dispatch:", workflow)
        self.assertIn('- "v*"', workflow)
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

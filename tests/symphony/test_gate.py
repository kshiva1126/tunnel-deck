"""Offline behavior harness: real gate/hooks, fake GitHub, Git and Codex."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / "scripts/symphony"
SPEC = importlib.util.spec_from_file_location("gate", SCRIPTS / "gate.py")
gate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gate)

FAKE = r'''#!/usr/bin/env python3
import json, os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
root = pathlib.Path(os.environ["FAKE_ROOT"])
with (root / "calls").open("a") as f:
    f.write(name + " " + " ".join(args) + "\n")
config = json.loads((root / "fixture.json").read_text())
if name == "gh":
    if args[0] == "api":
        method = args[args.index("--method") + 1]
        endpoint = next(a for a in args if a.startswith("repos/"))
        kind = "issue"
        if "dependencies/" in endpoint: kind = "dependencies"
        elif "/pulls?" in endpoint: kind = "prs"
        elif "/labels" in endpoint: kind = "labels"
        if config.get("hang") == kind:
            import time
            time.sleep(5)
        if config.get("fail") in (kind, method + ":" + kind):
            print("synthetic-secret-do-not-log", file=sys.stderr)
            sys.exit(1)
        if method != "GET":
            if method == "DELETE":
                config["issue"][0]["labels"] = [x for x in config["issue"][0]["labels"] if x["name"] != "agent-ready"]
            else:
                assert json.load(sys.stdin) == {"labels": ["blocked"]}
                config["issue"][0]["labels"].append({"name": "blocked"})
            (root / "fixture.json").write_text(json.dumps(config))
            sys.exit(0)
        assert "--paginate" in args and "--slurp" in args
        if kind in config.get("raw", {}):
            print(config["raw"][kind])
        elif config.get("deeply_nested") == kind:
            print("[" * 2000 + '"synthetic-secret-do-not-log"' + "]" * 2000)
        elif config.get("malformed") == kind:
            print("{synthetic-secret-do-not-log")
        else:
            print(json.dumps(config[kind]))
        sys.exit(0)
    if args[:2] == ["issue", "view"]: print("Fixture issue")
    elif args[:2] == ["pr", "create"]: print("https://github.com/kshiva1126/tunnel-deck/pull/999")
    elif args[:2] == ["pr", "list"]: pass
    elif args[:2] == ["issue", "edit"]: pass
    else: sys.exit(3)
elif name == "git":
    command = args[2:]
    if command[:2] == ["branch", "--show-current"]: print("symphony/issue-24")
    elif command[:2] == ["remote", "get-url"]: print("https://github.com/kshiva1126/tunnel-deck.git")
    elif command[:2] == ["rev-list", "--count"]: print("1")
    elif command[:1] == ["diff"]: print("fixture change")
    elif command[:1] not in (["status"], ["rev-parse"], ["push"]): sys.exit(4)
elif name == "codex":
    assert args == ["app-server"]
    assert all(k not in os.environ for k in ("SYMPHONY_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN", "SSH_AUTH_SOCK"))
    (root / "codex-ran").touch()
elif name == "setpriv":
    # Model command forwarding; this does not claim native UID isolation.
    os.environ.pop("SYMPHONY_AGENT_UID", None)
    os.execvp(args[3], args[3:])
'''


def dependency(number, state):
    return {"number": number, "state": state,
            "repository_url": "https://api.github.com/repos/kshiva1126/tunnel-deck"}


class HookTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.workspace = self.root / "GH-24"
        self.workspace.mkdir()
        # The real publish hook runs required Rust commands on a tiny offline
        # crate. Git/GitHub are fakes, so no push or external write can occur.
        (self.workspace / "Cargo.toml").write_text('[package]\nname="gate-fixture"\nversion="0.1.0"\nedition="2021"\n')
        (self.workspace / "src").mkdir()
        (self.workspace / "src/lib.rs").write_text('pub fn fixture() -> bool {\n    true\n}\n')
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        for name in ("gh", "git", "codex", "setpriv"):
            path = bin_dir / name
            path.write_text(FAKE)
            path.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}",
                        SYMPHONY_CONTROL_ROOT=str(ROOT), SYMPHONY_STATE_ROOT=str(self.root / "state"),
                        SYMPHONY_GITHUB_TOKEN="synthetic-secret-do-not-log",
                        GH_TOKEN="synthetic-secret-do-not-log", GITHUB_TOKEN="synthetic-secret-do-not-log",
                        SSH_AUTH_SOCK="synthetic-secret-do-not-log", FAKE_ROOT=str(self.root))
        self.env.pop("SYMPHONY_AGENT_UID", None)
        self.env.pop("SYMPHONY_AGENT_GID", None)
        self.config = {"issue": [{"number": 24, "state": "open", "labels": [{"name": "agent-ready"}]}],
                       "dependencies": [[]], "prs": [[]]}
        self.save()

    def save(self):
        (self.root / "fixture.json").write_text(json.dumps(self.config))

    def hook(self, mode):
        result = subprocess.run([sys.executable, str(SCRIPTS / "gate.py"), mode, str(self.workspace)],
                                env=self.env, text=True, capture_output=True, timeout=60)
        self.assertNotIn("synthetic-secret-do-not-log", result.stdout + result.stderr)
        return result

    def calls(self):
        path = self.root / "calls"
        return path.read_text() if path.exists() else ""

    def attempt(self):
        before = self.hook("before")
        if before.returncode == 0:
            subprocess.run([str(SCRIPTS / "codex.sh")], env=self.env, check=True, timeout=10)
        after = self.hook("after")  # Match Symphony's unconditional after hook.
        return before, after

    def assert_blocked(self):
        before, after = self.attempt()
        self.assertNotEqual(before.returncode, 0)
        self.assertEqual(after.returncode, 0, after.stderr)
        self.assertFalse((self.root / "codex-ran").exists())
        self.assertNotIn("git ", self.calls())
        self.assertNotIn("gh pr ", self.calls())
        calls = self.calls()
        self.attempt()
        self.assertEqual(calls, self.calls(), "latched retries must perform no external calls")
        self.assertTrue((self.root / "state/GH-24.stopped").exists())
        self.assertTrue((self.root / "state/halt").exists(),
                        "redispatch must stop the scheduler, not just fail another hook")
        return before

    def clear_stop(self):
        # Operator recovery happens only while the worker is stopped.
        (self.root / "state/GH-24.stopped").unlink()
        (self.root / "state/halt").unlink(missing_ok=True)

    def test_no_dependencies_runs_and_publishes(self):
        before, after = self.attempt()
        self.assertEqual(before.returncode, 0, before.stderr)
        self.assertEqual(after.returncode, 0, after.stderr)
        self.assertTrue((self.root / "codex-ran").exists())
        self.assertIn("push --set-upstream", self.calls())
        self.assertEqual(self.calls().count("gh pr create"), 1)
        calls = self.calls()
        self.attempt()
        self.assertEqual(calls, self.calls())

    def test_all_closed_pages_run_in_docker_command_mode(self):
        self.config["dependencies"] = [[dependency(22, "closed")], [dependency(23, "closed")]]
        self.save()
        # The fake models setpriv exec, not OS credential/process isolation.
        self.env.update(SYMPHONY_AGENT_UID=str(os.getuid()), SYMPHONY_AGENT_GID=str(os.getgid()))
        before, after = self.attempt()
        self.assertEqual(before.returncode, 0, before.stderr)
        self.assertEqual(after.returncode, 0, after.stderr)
        self.assertIn("setpriv --reuid=", self.calls())
        self.assertIn("gh pr create", self.calls())

    def test_open_dependencies_on_later_page(self):
        for count in (1, 2):
            with self.subTest(count=count):
                self.config["dependencies"] = [[dependency(22, "closed")],
                                                [dependency(30 + n, "open") for n in range(count)]]
                self.save()
                before = self.assert_blocked()
                for n in range(count):
                    self.assertIn(f"kshiva1126/tunnel-deck#{30 + n}", before.stderr)
                self.clear_stop()

    def test_api_error_and_invalid_json_fail_closed(self):
        for kind in ("issue", "dependencies", "prs"):
            for failure in ("fail", "malformed"):
                with self.subTest(kind=kind, failure=failure):
                    self.config[failure] = kind
                    self.save()
                    self.assert_blocked()
                    self.clear_stop()
                    del self.config[failure]

    def test_invalid_dependency_schema_fails_closed(self):
        for bad in ({}, {"state": "closed"}, dependency(22, "unknown"), None):
            with self.subTest(bad=bad):
                self.config["dependencies"] = [[bad]]
                self.save()
                self.assert_blocked()
                self.clear_stop()

    def test_excessive_json_nesting_latches_and_suppresses_retries(self):
        for kind in ("issue", "dependencies", "prs"):
            with self.subTest(kind=kind):
                self.config["deeply_nested"] = kind
                self.save()
                before = self.assert_blocked()
                self.assertNotIn("Traceback", before.stderr)
                self.assertIn("invalid data", before.stderr)
                self.clear_stop()

    def test_invalid_issue_number_fails_closed(self):
        for number in (24.0, "24", True, None, 25):
            with self.subTest(number=number):
                self.config["issue"][0]["number"] = number
                self.save()
                self.assert_blocked()
                self.clear_stop()

    def test_duplicate_fields_cannot_hide_ineligible_state(self):
        responses = {
            "issue": '[{"number":24,"state":"closed","state":"open",'
                     '"labels":[{"name":"agent-ready"}]}]',
            "dependencies": '[[{"number":22,"state":"open","state":"closed",'
                            '"repository_url":"https://api.github.com/repos/kshiva1126/tunnel-deck"}]]',
        }
        for kind, response in responses.items():
            with self.subTest(kind=kind):
                self.config["raw"] = {kind: response}
                self.save()
                before = self.assert_blocked()
                self.assertIn("invalid data", before.stderr)
                self.clear_stop()

    def test_invalid_page_shapes_fail_closed(self):
        for kind in ("issue", "dependencies", "prs"):
            original = self.config[kind]
            for pages in ([], {}, [None], [{"message": "synthetic-secret-do-not-log"}]):
                with self.subTest(kind=kind, pages=pages):
                    self.config[kind] = pages
                    self.save()
                    self.assert_blocked()
                    self.clear_stop()
            self.config[kind] = original

    def test_non_json_constants_latch_and_suppress_retries(self):
        for constant in ("NaN", "Infinity", "-Infinity"):
            for kind in ("issue", "dependencies", "prs"):
                with self.subTest(constant=constant, kind=kind):
                    if kind == "issue":
                        response = ('[{"number":24,"state":"open",'
                                    '"labels":[{"name":"agent-ready"}],'
                                    '"invalid":' + constant + '}]')
                    elif kind == "dependencies":
                        response = ('[[{"number":22,"state":"closed",'
                                    '"repository_url":"https://api.github.com/repos/kshiva1126/tunnel-deck",'
                                    '"invalid":' + constant + '}]]')
                    else:
                        response = '[[' + constant + ']]'
                    self.config["raw"] = {kind: response}
                    self.save()
                    before = self.assert_blocked()
                    self.assertIn("invalid data", before.stderr)
                    self.clear_stop()

    def test_verify_is_read_only_on_success_and_refusal(self):
        for dependencies, status in (([], 0), ([dependency(22, "open")], 1)):
            with self.subTest(dependencies=dependencies):
                self.config["dependencies"] = [dependencies]
                self.save()
                fixture = (self.root / "fixture.json").read_bytes()
                self.assertEqual(self.hook("verify").returncode, status)
                self.assertEqual((self.root / "fixture.json").read_bytes(), fixture)
                self.assertFalse((self.root / "state").exists())
                self.assertFalse((self.root / "codex-ran").exists())
                for call in self.calls().splitlines():
                    self.assertTrue(call.startswith("gh api "), call)
                    self.assertIn("--method GET ", call)

    def test_closed_review_blocked_and_unqueued_issues(self):
        for state, labels in (("closed", ["agent-ready"]), ("open", ["agent-ready", "human-review"]),
                              ("open", ["agent-ready", "blocked"]), ("open", [])):
            with self.subTest(state=state, labels=labels):
                self.config["issue"][0].update(state=state, labels=[{"name": name} for name in labels])
                self.save()
                self.assert_blocked()
                self.clear_stop()

    def test_any_pr_history_prevents_reprocessing(self):
        for state, merged in (("open", None), ("closed", None), ("closed", "2026-09-22T00:00:00Z")):
            with self.subTest(state=state, merged=merged):
                self.config["prs"] = [[], [{"number": 99, "state": state, "merged_at": merged}]]
                self.save()
                self.assert_blocked()
                self.clear_stop()

    def test_merge_between_before_and_after_prevents_publish(self):
        self.assertEqual(self.hook("before").returncode, 0)
        self.config["prs"] = [[{"state": "closed", "merged_at": "now"}]]
        self.save()
        self.assertNotEqual(self.hook("after").returncode, 0)
        self.assertNotIn("push", self.calls())
        self.assertNotIn("gh pr create", self.calls())

    def test_after_without_permit_never_calls_publish(self):
        self.assertEqual(self.hook("after").returncode, 0)
        self.assertEqual(self.calls(), "")

    def test_hook_launch_failure_latches_and_suppresses_retries(self):
        control = self.root / "synthetic-secret-do-not-log"
        hooks = control / "scripts/symphony"
        hooks.mkdir(parents=True)
        for mode in ("before", "after"):
            for failure in ("missing", "non-executable"):
                with self.subTest(mode=mode, failure=failure):
                    self.env["SYMPHONY_CONTROL_ROOT"] = str(ROOT)
                    if mode == "after":
                        self.assertEqual(self.hook("before").returncode, 0)
                    self.env["SYMPHONY_CONTROL_ROOT"] = str(control)
                    hook = hooks / f"{mode}_run.sh"
                    if failure == "non-executable":
                        hook.write_text("#!/bin/sh\nexit 0\n")
                        hook.chmod(0o600)
                    result = self.hook(mode)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(f"{mode}_run hook could not start", result.stderr)
                    stopped = self.root / "state/GH-24.stopped"
                    self.assertTrue(stopped.exists())
                    self.assertNotIn("synthetic-secret", stopped.read_text())
                    self.assertFalse((self.root / "state/GH-24.permit").exists())
                    calls = self.calls()
                    for _ in range(2):
                        before, after = self.attempt()
                        self.assertNotEqual(before.returncode, 0)
                        self.assertEqual(after.returncode, 0)
                    self.assertEqual(calls, self.calls())
                    self.assertTrue((self.root / "state/halt").exists())
                    self.assertFalse((self.root / "codex-ran").exists())
                    self.assertNotIn("push", calls)
                    self.assertNotIn("gh pr ", calls)
                    self.clear_stop()
                    hook.unlink(missing_ok=True)
                    self.save()  # Restore queue labels for the next scenario.
                    (self.root / "calls").unlink()

    def test_label_failure_requests_global_halt_once(self):
        self.config.update(fail="labels", dependencies=[[dependency(22, "open")]])
        self.save()
        self.assert_blocked()
        self.assertTrue((self.root / "state/halt").exists())
        result = subprocess.run([sys.executable, str(SCRIPTS / "worker.py"), "codex", "app-server"],
                                env=self.env, capture_output=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "codex-ran").exists())

    def test_manual_recovery_requires_fresh_dependency_check(self):
        self.config["dependencies"] = [[dependency(22, "open")]]
        self.save()
        self.assert_blocked()
        self.config["dependencies"] = [[dependency(22, "closed")]]
        self.save()  # Operator restores agent-ready and removes blocked.
        self.assertNotEqual(self.hook("before").returncode, 0)
        self.clear_stop()
        self.assertEqual(self.hook("before").returncode, 0)

    def test_pre_push_verify_rejects_new_dependency(self):
        self.config["dependencies"] = [[dependency(22, "open")]]
        self.save()
        result = subprocess.run([str(SCRIPTS / "after_run.sh"), str(self.workspace)], env=self.env,
                                text=True, capture_output=True, timeout=60)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("/dependencies/blocked_by", self.calls())
        self.assertNotIn("push", self.calls())
        self.assertNotIn("gh pr create", self.calls())

    def test_open_dependency_also_blocks_docker_command_mode(self):
        self.env.update(SYMPHONY_AGENT_UID=str(os.getuid()), SYMPHONY_AGENT_GID=str(os.getgid()))
        self.config["dependencies"] = [[dependency(22, "open")]]
        self.save()
        self.assert_blocked()
        self.assertNotIn("setpriv", self.calls())

    def test_timeout_latches_and_skips_publish(self):
        self.config["hang"] = "dependencies"
        self.save()
        with patch.dict(os.environ, self.env, clear=True), patch.object(gate, "API_TIMEOUT", 0.2), \
                patch.object(sys, "argv", ["gate.py", "before", str(self.workspace)]), \
                contextlib.redirect_stderr(io.StringIO()) as diagnostics:
            self.assertEqual(gate.main(), 1)
        self.assertNotIn("synthetic-secret", diagnostics.getvalue())
        self.assertTrue((self.root / "state/GH-24.stopped").exists())
        self.assertEqual(self.hook("after").returncode, 0)
        self.assertNotIn("git ", self.calls())
        calls = self.calls()
        self.assertNotEqual(self.hook("before").returncode, 0)
        self.assertEqual(calls, self.calls())

    def test_failed_blocked_label_does_not_restore_queue_or_halt(self):
        self.config.update(fail="POST:labels", dependencies=[[dependency(22, "open")]])
        self.save()
        before, after = self.attempt()
        self.assertNotEqual(before.returncode, 0)
        self.assertEqual(after.returncode, 0)
        issue = json.loads((self.root / "fixture.json").read_text())["issue"][0]
        self.assertNotIn({"name": "agent-ready"}, issue["labels"])
        self.assertFalse((self.root / "state/halt").exists())
        self.assert_blocked()

    def test_relabel_stopped_issue_halts_worker_without_external_calls(self):
        self.config["dependencies"] = [[dependency(22, "open")]]
        self.save()
        self.assertNotEqual(self.hook("before").returncode, 0)
        self.assertFalse((self.root / "state/halt").exists())
        # Relabeling without clearing the durable stop must not create a loop.
        self.config["dependencies"] = [[dependency(22, "closed")]]
        self.save()
        calls = self.calls()
        self.assert_blocked()
        self.assertEqual(calls, self.calls())
        result = subprocess.run([sys.executable, str(SCRIPTS / "worker.py"), "codex", "app-server"],
                                env=self.env, capture_output=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "codex-ran").exists())

    def test_unsafe_state_directory_refuses_without_external_effects(self):
        state = self.root / "state"
        state.mkdir(mode=0o700)
        state.chmod(0o755)
        self.assertNotEqual(self.hook("before").returncode, 0)
        self.assertEqual(self.calls(), "")

    def test_worker_exits_when_halt_appears(self):
        self.root.joinpath("state").mkdir(mode=0o700)
        code = ('import os, pathlib, time; '
                'pathlib.Path(os.environ["SYMPHONY_STATE_ROOT"], "halt").touch(); time.sleep(30)')
        result = subprocess.run([sys.executable, str(SCRIPTS / "worker.py"), sys.executable, "-c", code],
                                env=self.env, capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 1)


class ApiTimeoutTests(unittest.TestCase):
    def test_actual_timeout_and_redaction(self):
        # Exercise subprocess timeout/kill, without waiting the production 30s.
        with tempfile.TemporaryDirectory() as directory:
            fake = Path(directory) / "gh"
            fake.write_text('#!/bin/sh\nprintf synthetic-secret-do-not-log >&2\nexec sleep 5\n')
            fake.chmod(0o755)
            with patch.dict(os.environ, PATH=f"{directory}:{os.environ['PATH']}",
                            SYMPHONY_GITHUB_TOKEN="synthetic-secret-do-not-log"), patch.object(gate, "API_TIMEOUT", 0.05):
                with self.assertRaisesRegex(gate.Refused, "timed out") as error:
                    gate.api("repos/example/repo/issues/1")
                self.assertNotIn("synthetic-secret", str(error.exception))


if __name__ == "__main__":
    unittest.main()

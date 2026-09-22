"""Offline behavior harness: real gate/hooks, fake GitHub, Git and Codex."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shlex
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
REPORT_SPEC = importlib.util.spec_from_file_location("run_report", SCRIPTS / "run_report.py")
run_report = importlib.util.module_from_spec(REPORT_SPEC)
REPORT_SPEC.loader.exec_module(run_report)

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
        endpoint = next(a for a in args if a == "user" or a.startswith("repos/"))
        if endpoint == "user":
            assert method == "GET"
            assert "--paginate" not in args and "--slurp" not in args
            print(json.dumps({"login": config.get("actor", "symphony-publisher")}))
            sys.exit(0)
        kind = "issue"
        if "dependencies/" in endpoint: kind = "dependencies"
        elif "/pulls?" in endpoint: kind = "prs"
        elif "/labels" in endpoint: kind = "labels"
        elif "/comments" in endpoint: kind = "comments"
        # Change GitHub only at the final pre-push query, after both admission
        # checks succeeded and the publish hook completed its Rust checks.
        change = config.get("pre_push_change")
        if change and method == "GET" and kind == change["kind"]:
            count = sum("--method GET " in line and endpoint in line
                        for line in (root / "calls").read_text().splitlines())
            if count == 3:
                config.update(change["response"])
        if config.get("hang") in (kind, method + ":" + kind):
            if kind == "labels":
                # Even an indeterminate remote write must start only after
                # the local stop is durable enough to survive redispatch.
                assert (pathlib.Path(os.environ["SYMPHONY_STATE_ROOT"]) / "GH-24.stopped").is_file()
                (root / "label-timeout-started").touch()
            import time
            time.sleep(5)
        if config.get("fail") in (kind, method + ":" + kind):
            print("synthetic-secret-do-not-log", file=sys.stderr)
            sys.exit(1)
        if kind == "comments" and method != "GET":
            body = json.load(sys.stdin)["body"]
            if method == "POST":
                comment = {"id": 700, "body": body,
                           "user": {"login": config.get("actor", "symphony-publisher")}}
                config["comments"][0].append(comment)
            else:
                comment = next(item for page in config["comments"] for item in page
                               if endpoint.endswith("/" + str(item["id"])))
                comment["body"] = body
            (root / "fixture.json").write_text(json.dumps(config))
            print(json.dumps(comment))
            sys.exit(0)
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
    while command[:1] == ["-c"]:
        command = command[2:]
    if command[:2] == ["branch", "--show-current"]: print("symphony/issue-24")
    elif command[:2] == ["remote", "get-url"]: print("https://github.com/kshiva1126/tunnel-deck.git")
    elif command[:2] == ["rev-list", "--count"]: print("1")
    elif command[:1] == ["diff"]: print("fixture change")
    elif command[:1] == ["status"] and config.get("dirty"): print("?? unfinished")
    elif command[:2] == ["rev-parse", "HEAD"]: print("a" * 40)
    elif command[:1] not in (["status"], ["rev-parse"], ["push"]): sys.exit(4)
elif name == "codex":
    assert args == ["app-server", "-c", 'model="gpt-5.6-sol"']
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


class JsonNestingTests(unittest.TestCase):
    def test_limit_ignores_brackets_inside_strings(self):
        gate.reject_excessive_json_nesting(
            json.dumps({"body": "[" * 200 + '\\"' + "]" * 200})
        )

    def test_limit_rejects_the_first_level_above_the_boundary(self):
        allowed = "[" * gate.MAX_JSON_NESTING + "0" + "]" * gate.MAX_JSON_NESTING
        gate.reject_excessive_json_nesting(allowed)
        refused = "[" + allowed + "]"
        with self.assertRaises(ValueError):
            gate.reject_excessive_json_nesting(refused)


class RunReportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.workspace = Path(self.temp.name)
        run_report.write_initial(self.workspace, 26, "GH-26-attempt-1")

    def test_all_terminal_statuses_are_valid(self):
        path = run_report.report_path(self.workspace)
        for status in ("completed", "failed", "blocked", "interrupted", "publish_failed"):
            with self.subTest(status=status):
                report = json.loads(path.read_text())
                report["status"] = status
                path.write_text(json.dumps(report))
                loaded = run_report.load_report(self.workspace, 26)
                self.assertEqual(loaded["status"], status)
                self.assertIn(f"`{status}`", run_report.render_attempt(loaded))

    def test_rejects_wrong_issue_oversize_malformed_and_credentials(self):
        path = run_report.report_path(self.workspace)
        original = path.read_text()
        wrong_types = []
        for field, value in (("run_id", []), ("status", []), ("commit", 7),
                             ("pull_request", {})):
            report = json.loads(original)
            report[field] = value
            wrong_types.append(json.dumps(report))
        for field, value in (("facts", "not-a-list"), ("decisions", [7]),
                             ("alternatives", [{"reason": 7}]),
                             ("tests", [{"command": None}]),
                             ("unknowns", [{"risk": ["nested"]}])):
            report = json.loads(original)
            report[field] = value
            wrong_types.append(json.dumps(report))
        cases = (
            original.replace('"issue_number": 26', '"issue_number": 27'),
            "{" + "x" * run_report.MAX_REPORT_BYTES,
            "not json",
            original.replace("Codex turn started", "github_pat_" + "x" * 30),
            *wrong_types,
        )
        for content in cases:
            with self.subTest(content=content[:20]):
                path.write_text(content)
                with self.assertRaises(run_report.ReportError):
                    run_report.load_report(self.workspace, 26)

    def test_create_update_and_same_run_deduplication(self):
        report = run_report.load_report(self.workspace, 26)
        calls = []
        comments = []

        def fake_api(endpoint, method="GET", body=None, paginate=True):
            calls.append((method, endpoint))
            if endpoint == "user":
                self.assertFalse(paginate)
                return {"login": "symphony-publisher"}
            if method == "GET":
                return [comments]
            if method == "POST":
                comments.append({"id": 9, "body": body["body"],
                                 "user": {"login": "symphony-publisher"}})
                return comments[0]
            comments[0]["body"] = body["body"]
            return comments[0]

        with patch.object(run_report, "gh_api", fake_api):
            run_report.upsert(report)
            report["status"] = "completed"
            run_report.upsert(report)
        self.assertEqual([method for method, _ in calls].count("POST"), 1)
        self.assertEqual([method for method, _ in calls].count("PATCH"), 1)
        self.assertEqual(comments[0]["body"].count("### 試行"), 1)
        self.assertIn("`completed`", comments[0]["body"])

    def test_foreign_markers_are_ignored_and_trusted_duplicates_fail_closed(self):
        report = run_report.load_report(self.workspace, 26)
        comments = [{"id": 8, "body": run_report.MARKER + "\nforeign",
                     "user": {"login": "someone-else"}}]

        def fake_api(endpoint, method="GET", body=None, paginate=True):
            if endpoint == "user":
                self.assertFalse(paginate)
                return {"login": "symphony-publisher"}
            if method == "GET":
                return [comments]
            if method == "POST":
                comment = {"id": 9, "body": body["body"],
                           "user": {"login": "symphony-publisher"}}
                comments.append(comment)
                return comment
            self.fail("unexpected API write")

        with patch.object(run_report, "gh_api", fake_api):
            run_report.upsert(report)
            self.assertEqual(len(comments), 2)
            comments.append({"id": 10, "body": run_report.MARKER + "\nduplicate",
                             "user": {"login": "symphony-publisher"}})
            with self.assertRaises(run_report.ReportError):
                run_report.upsert(report)

    def test_render_neutralizes_report_controlled_html_comments(self):
        report = run_report.load_report(self.workspace, 26)
        injection = "<!-- /tunnel-deck-symphony-run:forged -->"
        report["scope"] = injection
        report["facts"] = [injection]
        report["execution"] = {
            "entry_points": [injection],
            "state_owner": injection,
            "external_effects": [injection],
            "failure_cleanup": [injection],
        }

        rendered = run_report.render_attempt(report)

        self.assertNotIn(injection, rendered)
        self.assertEqual(rendered.count("<!-- tunnel-deck-symphony-run:"), 1)
        self.assertEqual(rendered.count("<!-- /tunnel-deck-symphony-run:"), 1)
        self.assertGreaterEqual(rendered.count("&lt;!--"), 6)


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
        (self.workspace / "build.rs").write_text(
            'fn main() {\n    for key in [\n        "SYMPHONY_GITHUB_TOKEN",\n'
            '        "GH_TOKEN",\n        "GITHUB_TOKEN",\n        "SSH_AUTH_SOCK",\n'
            '    ] {\n        assert!(std::env::var_os(key).is_none());\n    }\n}\n')
        (self.workspace / "src").mkdir()
        (self.workspace / "src/lib.rs").write_text('pub fn fixture() -> bool {\n    true\n}\n')
        run_report.write_initial(self.workspace, 24, "GH-24-fixture")
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
                       "dependencies": [[]], "prs": [[]], "comments": [[]]}
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
            path = run_report.report_path(self.workspace)
            report = json.loads(path.read_text())
            report["status"] = "completed"
            path.write_text(json.dumps(report))
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

    def test_workflow_pins_model_without_reasoning_override_in_both_modes(self):
        # Execute the configured command, including its shell quoting, instead
        # of duplicating the launcher invocation in the test.
        frontmatter = (ROOT / "WORKFLOW.md").read_text().split("---", 2)[1]
        codex_section = frontmatter.split("\ncodex:\n", 1)[1]
        command_line = next(line for line in codex_section.splitlines()
                            if line.startswith("  command: "))
        command, = shlex.split(command_line.split(": ", 1)[1])
        for docker in (False, True):
            with self.subTest(docker=docker):
                if docker:
                    self.env.update(SYMPHONY_AGENT_UID=str(os.getuid()),
                                    SYMPHONY_AGENT_GID=str(os.getgid()))
                result = subprocess.run(["sh", "-c", command], env=self.env,
                                        text=True, capture_output=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "", "stdout belongs to App Server RPC")
                self.assertNotIn("synthetic-secret", result.stderr)
                self.assertTrue((self.root / "codex-ran").exists())
                self.assertIn('codex app-server -c model="gpt-5.6-sol"\n', self.calls())
                self.assertEqual("setpriv --reuid=" in self.calls(), docker)
                (self.root / "codex-ran").unlink()
                (self.root / "calls").unlink()

    def test_no_dependencies_runs_and_publishes(self):
        before, after = self.attempt()
        self.assertEqual(before.returncode, 0, before.stderr)
        self.assertEqual(after.returncode, 0, after.stderr)
        self.assertTrue((self.root / "codex-ran").exists())
        self.assertIn("push --no-verify https://github.com/kshiva1126/tunnel-deck.git", self.calls())
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

    def test_admission_change_during_turn_consumes_permit_and_stops_publish(self):
        for change in ("open_dependency", "fail", "malformed"):
            with self.subTest(change=change):
                self.save()
                self.assertEqual(self.hook("before").returncode, 0)
                subprocess.run([str(SCRIPTS / "codex.sh")], env=self.env,
                               check=True, timeout=10)
                self.assertTrue((self.root / "codex-ran").exists())
                if change == "open_dependency":
                    self.config["dependencies"] = [[dependency(22, "open")]]
                else:
                    self.config[change] = "dependencies"
                self.save()
                # Only effects after the turn matter: workspace preparation
                # legitimately used Git before the dependency changed.
                (self.root / "calls").unlink()
                result = self.hook("after")
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((self.root / "state/GH-24.permit").exists())
                self.assertTrue((self.root / "state/GH-24.stopped").exists())
                self.assertNotIn("git ", self.calls())
                self.assertNotIn("gh pr ", self.calls())
                calls = self.calls()
                self.assertEqual(self.hook("after").returncode, 0)
                (self.root / "codex-ran").unlink()
                before, after = self.attempt()
                self.assertNotEqual(before.returncode, 0)
                self.assertEqual(after.returncode, 0)
                self.assertFalse((self.root / "codex-ran").exists())
                self.assertEqual(calls, self.calls())
                self.assertTrue((self.root / "state/halt").exists())
                self.clear_stop()
                self.config.pop(change, None)
                self.config["dependencies"] = [[]]

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

    def test_missing_trusted_credential_halts_without_fallback_in_both_modes(self):
        for docker in (False, True):
            for token in (None, ""):
                with self.subTest(docker=docker, token=token):
                    if docker:
                        self.env.update(SYMPHONY_AGENT_UID=str(os.getuid()),
                                        SYMPHONY_AGENT_GID=str(os.getgid()))
                    if token is None:
                        self.env.pop("SYMPHONY_GITHUB_TOKEN", None)
                    else:
                        self.env["SYMPHONY_GITHUB_TOKEN"] = token
                    # GH_TOKEN and GITHUB_TOKEN remain set: neither authorizes
                    # admission when the trusted workflow credential is absent.
                    before = self.assert_blocked()
                    self.assertIn("GitHub credential unavailable", before.stderr)
                    self.assertEqual(self.calls(), "")
                    self.assertFalse((self.root / "state/GH-24.permit").exists())
                    result = subprocess.run(
                        [sys.executable, str(SCRIPTS / "worker.py"), "codex", "app-server"],
                        env=self.env, text=True, capture_output=True, timeout=10)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertNotIn("synthetic-secret", result.stdout + result.stderr)
                    self.assertFalse((self.root / "codex-ran").exists())
                    self.assertEqual(self.calls(), "")
                    self.clear_stop()

    def test_manual_recovery_requires_fresh_dependency_check(self):
        for docker in (False, True):
            with self.subTest(docker=docker):
                if docker:
                    self.env.update(SYMPHONY_AGENT_UID=str(os.getuid()),
                                    SYMPHONY_AGENT_GID=str(os.getgid()))
                self.config["dependencies"] = [[dependency(22, "open")]]
                self.save()
                self.assert_blocked()
                self.config["dependencies"] = [[dependency(22, "closed")]]
                self.save()  # Operator restores agent-ready and removes blocked.
                calls = self.calls()
                self.assertNotEqual(self.hook("before").returncode, 0)
                self.assertEqual(calls, self.calls(), "relabeling alone cannot resume work")

                self.clear_stop()  # Complete documented recovery with worker stopped.
                (self.root / "calls").unlink()
                before, after = self.attempt()
                self.assertEqual(before.returncode, 0, before.stderr)
                self.assertEqual(after.returncode, 0, after.stderr)
                self.assertTrue((self.root / "codex-ran").exists())
                self.assertIn("/dependencies/blocked_by", self.calls())
                self.assertEqual(self.calls().count(
                    "push --no-verify https://github.com/kshiva1126/tunnel-deck.git"), 1)
                self.assertEqual(self.calls().count("gh pr create"), 1)
                self.assertFalse((self.root / "state/GH-24.permit").exists())
                self.assertIn("published", (self.root / "state/GH-24.stopped").read_text())

                # Successful recovery is still a one-shot publication.
                (self.root / "codex-ran").unlink()
                calls = self.calls()
                before, after = self.attempt()
                self.assertNotEqual(before.returncode, 0)
                self.assertEqual(after.returncode, 0)
                self.assertFalse((self.root / "codex-ran").exists())
                self.assertEqual(calls, self.calls())
                self.assertTrue((self.root / "state/halt").exists())
                self.clear_stop()
                (self.root / "calls").unlink()

    def test_pre_push_verify_rejects_new_dependency(self):
        self.config["dependencies"] = [[dependency(22, "open")]]
        self.save()
        path = run_report.report_path(self.workspace)
        report = json.loads(path.read_text())
        report["status"] = "completed"
        path.write_text(json.dumps(report))
        result = subprocess.run([str(SCRIPTS / "after_run.sh"), str(self.workspace)], env=self.env,
                                text=True, capture_output=True, timeout=60)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("/dependencies/blocked_by", self.calls())
        self.assertNotIn("push", self.calls())
        self.assertNotIn("gh pr create", self.calls())

    def test_workspace_failure_updates_the_single_report_comment(self):
        self.config["dirty"] = True
        self.save()
        path = run_report.report_path(self.workspace)
        report = json.loads(path.read_text())
        report["status"] = "completed"
        path.write_text(json.dumps(report))
        result = subprocess.run([str(SCRIPTS / "after_run.sh"), str(self.workspace)], env=self.env,
                                text=True, capture_output=True, timeout=60)
        self.assertNotEqual(result.returncode, 0)
        comments = json.loads((self.root / "fixture.json").read_text())["comments"][0]
        self.assertEqual(len(comments), 1)
        self.assertEqual(comments[0]["body"].count("### 試行"), 1)
        self.assertIn("`failed`", comments[0]["body"])

    def test_comment_api_failure_is_bounded_and_leaves_code_untouched(self):
        self.config["fail"] = "comments"
        self.save()
        cargo = (self.workspace / "Cargo.toml").read_bytes()
        result = subprocess.run([str(SCRIPTS / "after_run.sh"), str(self.workspace)], env=self.env,
                                text=True, capture_output=True, timeout=60)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls().count("/comments?per_page=100"), 2)
        self.assertNotIn("push", self.calls())
        self.assertEqual((self.workspace / "Cargo.toml").read_bytes(), cargo)

    def test_open_dependency_also_blocks_docker_command_mode(self):
        self.env.update(SYMPHONY_AGENT_UID=str(os.getuid()), SYMPHONY_AGENT_GID=str(os.getgid()))
        self.config["dependencies"] = [[dependency(22, "open")]]
        self.save()
        self.assert_blocked()
        self.assertNotIn("setpriv", self.calls())

    def test_pre_push_change_latches_without_remote_publication_in_both_modes(self):
        changes = (
            ("dependencies", {"dependencies": [[dependency(22, "open")]]}),
            ("dependencies", {"fail": "dependencies"}),
            ("dependencies", {"malformed": "dependencies"}),
            ("prs", {"prs": [[{"number": 99, "state": "closed",
                                 "merged_at": "2026-09-22T00:00:00Z"}]]}),
        )
        for docker in (False, True):
            for kind, response in changes:
                with self.subTest(docker=docker, response=response):
                    if docker:
                        self.env.update(SYMPHONY_AGENT_UID=str(os.getuid()),
                                        SYMPHONY_AGENT_GID=str(os.getgid()))
                    self.config["pre_push_change"] = {"kind": kind, "response": response}
                    self.save()
                    before, after = self.attempt()
                    self.assertEqual(before.returncode, 0, before.stderr)
                    self.assertTrue((self.root / "codex-ran").exists())
                    self.assertNotEqual(after.returncode, 0)
                    # The real publish hook reached validation and its final
                    # gate; the earlier after admission did not reject it.
                    self.assertIn("rev-list --count", self.calls())
                    self.assertEqual(sum("--method GET " in line and
                                         ("/dependencies/" if kind == "dependencies" else "/pulls?") in line
                                         for line in self.calls().splitlines()), 3)
                    self.assertNotIn("push --set-upstream", self.calls())
                    self.assertNotIn("gh pr ", self.calls())
                    self.assertFalse((self.root / "state/GH-24.permit").exists())
                    self.assertTrue((self.root / "state/GH-24.stopped").exists())
                    calls = self.calls()
                    (self.root / "codex-ran").unlink()
                    self.assertEqual(self.hook("after").returncode, 0)
                    for _ in range(2):
                        before, after = self.attempt()
                        self.assertNotEqual(before.returncode, 0)
                        self.assertEqual(after.returncode, 0)
                    self.assertFalse((self.root / "codex-ran").exists())
                    self.assertEqual(calls, self.calls())
                    self.assertTrue((self.root / "state/halt").exists())
                    self.clear_stop()
                    (self.root / "calls").unlink()

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

    def test_label_timeouts_preserve_stop_and_suppress_redispatch_in_both_modes(self):
        for docker in (False, True):
            for method in ("DELETE", "POST"):
                with self.subTest(docker=docker, method=method):
                    if docker:
                        self.env.update(SYMPHONY_AGENT_UID=str(os.getuid()),
                                        SYMPHONY_AGENT_GID=str(os.getgid()))
                    self.config.update(hang=method + ":labels",
                                       dependencies=[[dependency(22, "open")]])
                    self.save()
                    with patch.dict(os.environ, self.env, clear=True), \
                            patch.object(gate, "API_TIMEOUT", 1), \
                            patch.object(sys, "argv", ["gate.py", "before", str(self.workspace)]), \
                            contextlib.redirect_stderr(io.StringIO()) as diagnostics:
                        self.assertEqual(gate.main(), 1)
                    self.assertTrue((self.root / "label-timeout-started").exists())
                    self.assertNotIn("synthetic-secret", diagnostics.getvalue())
                    self.assertTrue((self.root / "state/GH-24.stopped").exists())
                    self.assertFalse((self.root / "state/GH-24.permit").exists())
                    self.assertEqual((self.root / "state/halt").exists(), method == "DELETE")
                    labels = json.loads((self.root / "fixture.json").read_text())["issue"][0]["labels"]
                    self.assertEqual({"name": "agent-ready"} in labels, method == "DELETE")
                    self.assertNotIn({"name": "blocked"}, labels)
                    calls = self.calls()
                    self.assertEqual(calls.count("--method DELETE"), 1)
                    self.assertEqual(calls.count("--method POST"), int(method == "POST"))
                    self.assert_blocked()
                    self.assertEqual(calls, self.calls())
                    result = subprocess.run(
                        [sys.executable, str(SCRIPTS / "worker.py"), "codex", "app-server"],
                        env=self.env, capture_output=True, timeout=10)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(calls, self.calls())
                    self.assertFalse((self.root / "codex-ran").exists())
                    self.clear_stop()
                    (self.root / "calls").unlink()
                    (self.root / "label-timeout-started").unlink()

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

"""Behavior tests for the trusted post-publication state machine."""

import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SCRIPTS = ROOT / "scripts/symphony"
sys.path.insert(0, str(SCRIPTS))
SPEC = importlib.util.spec_from_file_location("review", SCRIPTS / "review.py")
review = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(review)


def pr(sha="a" * 40, state="open", head="symphony/issue-28", fork=False):
    return {"number": 99, "state": state, "merged": False, "body": "Refs #28",
            "base": {"ref": "main", "sha": "d" * 40,
                     "repo": {"full_name": review.REPO}},
            "head": {"ref": head, "sha": sha,
                     "repo": {"full_name": "someone/fork" if fork else review.REPO}}}


class ValidationTests(unittest.TestCase):
    def test_accepts_only_the_expected_open_same_repository_pr_and_sha(self):
        with patch.object(review, "one", return_value=pr()):
            self.assertEqual(review.expected_pr(28, 99, "a" * 40)["head"]["sha"], "a" * 40)
        for value in (pr(state="closed"), pr(head="other"), pr(fork=True), pr("b" * 40)):
            with self.subTest(value=value), patch.object(review, "one", return_value=value):
                with self.assertRaises(review.ReviewStopped):
                    review.expected_pr(28, 99, "a" * 40)

    def test_required_checks_fail_closed_when_missing_or_failed(self):
        success = {"id": 1, "name": "Linux", "status": "completed", "conclusion": "success"}
        with patch.dict(os.environ, {"SYMPHONY_REQUIRED_CHECKS": "Linux,macOS"}), \
                patch.object(review, "one", return_value={"check_runs": [success]}):
            self.assertEqual(review.check_state("a" * 40)[0], "pending")
        failed = {"id": 2, "name": "macOS", "status": "completed", "conclusion": "failure",
                  "output": {"title": "test failed", "summary": "bounded diagnostic"}}
        with patch.dict(os.environ, {"SYMPHONY_REQUIRED_CHECKS": "Linux,macOS"}), \
                patch.object(review, "one", return_value={"check_runs": [success, failed]}):
            state, failures, _ = review.check_state("a" * 40)
            self.assertEqual(state, "failed")
            self.assertEqual(review.failed_logs(failures)[0]["check"], "macOS")
        for value in ("", ",,", "Linux,Linux"):
            with self.subTest(value=value), patch.dict(
                    os.environ, {"SYMPHONY_REQUIRED_CHECKS": value}), \
                    patch.object(review, "one", return_value={"check_runs": [success]}):
                with self.assertRaises(review.ReviewStopped):
                    review.check_state("a" * 40)

    def test_context_rejects_credentials_before_codex(self):
        context = {"review": "github_pat_" + "x" * 30}
        with patch("subprocess.run") as run:
            with self.assertRaises(review.ReviewStopped):
                review.remediation(ROOT, context)
            run.assert_not_called()

    def test_remediation_pins_sol_and_strips_remote_credentials(self):
        completed = __import__("subprocess").CompletedProcess([], 0)
        with tempfile.TemporaryDirectory() as directory, \
                patch.dict(os.environ, {"SYMPHONY_GITHUB_TOKEN": "secret",
                                        "GH_TOKEN": "secret", "SSH_AUTH_SOCK": "/agent",
                                        "GIT_DIR": "/agent/repository"}), \
                patch("subprocess.run", return_value=completed) as run:
            review.remediation(Path(directory), {"kind": "untrusted_diagnostics"})
        command = run.call_args.args[0]
        child_env = run.call_args.kwargs["env"]
        self.assertEqual(command[:4], ["codex", "exec", "-c", 'model=gpt-5.6-sol'])
        for key in ("SYMPHONY_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN", "SSH_AUTH_SOCK",
                    "GIT_DIR"):
            self.assertNotIn(key, child_env)

    def test_push_uses_trusted_noninteractive_askpass(self):
        def completed(command, **_kwargs):
            output = "b" * 40 + "\n" if command[-2:] == ["rev-parse", "FETCH_HEAD"] else ""
            return __import__("subprocess").CompletedProcess(command, 0, stdout=output)

        with patch.dict(os.environ, {"SYMPHONY_CONTROL_ROOT": str(ROOT),
                                    "SYMPHONY_STATE_ROOT": str(ROOT),
                                    "SYMPHONY_GITHUB_TOKEN": "secret"}), \
                patch("subprocess.run", side_effect=completed) as run:
            review.push(ROOT, 28, "b" * 40, "a" * 40)
        command = run.call_args.args[0]
        self.assertEqual(command[0], "git")
        self.assertNotIn("setpriv", command)
        self.assertEqual(command[-3:], [
            "--force-with-lease=refs/heads/symphony/issue-28:" + "a" * 40,
            f"https://github.com/{review.REPO}.git",
            "b" * 40 + ":refs/heads/symphony/issue-28"])
        self.assertIn("core.hooksPath=/dev/null", command)
        self.assertIn("credential.helper=", command)
        push_env = run.call_args.kwargs["env"]
        self.assertEqual(push_env["GIT_TERMINAL_PROMPT"], "0")
        self.assertEqual(push_env["GIT_ASKPASS"],
                         str(ROOT / "scripts/symphony/git-askpass.sh"))

    def test_root_review_drops_to_the_configured_workspace_owner(self):
        with patch("os.geteuid", return_value=0), \
                patch.dict(os.environ, {"SYMPHONY_AGENT_UID": "1234",
                                        "SYMPHONY_AGENT_GID": "5678"}):
            self.assertEqual(review.agent_prefix(), ["setpriv", "--reuid=1234",
                             "--regid=5678", "--clear-groups"])
        with patch("os.geteuid", return_value=0), patch.dict(os.environ, {}, clear=True):
            with self.assertRaises(review.ReviewStopped):
                review.agent_prefix()

    def test_unresolved_coderabbit_line_comment_keeps_location_and_sha(self):
        response = {"data": {"repository": {"pullRequest": {"reviewThreads": {
            "pageInfo": {"hasNextPage": False}, "nodes": [{"isResolved": False,
                "comments": {"pageInfo": {"hasNextPage": False}, "nodes": [{
                    "body": "actionable finding", "path": "src/lib.rs", "line": 7,
                    "commit": {"oid": "a" * 40}, "author": {"login": "coderabbitai[bot]"}}]}}]}}}}}
        completed = __import__("subprocess").CompletedProcess([], 0, json.dumps(response), "")
        with patch.dict(os.environ, {"SYMPHONY_GITHUB_TOKEN": "test-token"}), \
                patch("subprocess.run", return_value=completed), \
                patch.object(review, "api", return_value=[[]]):
            self.assertEqual(review.review_findings(99, "a" * 40), [{
                "body": "actionable finding", "path": "src/lib.rs", "line": 7,
                "commit_sha": "a" * 40}])

    def test_current_head_actionable_review_body_is_a_finding(self):
        response = {"data": {"repository": {"pullRequest": {"reviewThreads": {
            "pageInfo": {"hasNextPage": False}, "nodes": []}}}}}
        completed = __import__("subprocess").CompletedProcess([], 0, json.dumps(response), "")
        reviews = [[{"id": 7, "body": "**Actionable comments posted: 2**",
                    "commit_id": "a" * 40, "user": {"login": "coderabbitai"}}]]
        with patch.dict(os.environ, {"SYMPHONY_GITHUB_TOKEN": "test-token"}), \
                patch("subprocess.run", return_value=completed), \
                patch.object(review, "api", return_value=reviews):
            findings = review.review_findings(99, "a" * 40)
        self.assertEqual(findings[0]["source"], "review_body")

    def test_failure_fingerprint_ignores_head_and_finding_commit(self):
        first = {"head_sha": "a" * 40, "failed_checks": [{"check": "Linux"}],
                 "review_findings": [{"body": "same", "commit_sha": "a" * 40}]}
        second = {"head_sha": "b" * 40, "failed_checks": [{"check": "Linux"}],
                  "review_findings": [{"body": "same", "commit_sha": "b" * 40}]}
        self.assertEqual(review.fingerprint(first), review.fingerprint(second))


class StateMachineTests(unittest.TestCase):
    def base_patches(self):
        return (patch.object(review, "expected_pr", return_value=pr()),
                patch.object(review, "open_dependencies", return_value=[]),
                patch.object(review, "risk_reason", return_value=None),
                patch.object(review, "review_findings", return_value=[]),
                patch.object(review, "review_state", return_value=(Path("state"), {
                    "issue": 28, "pull_request": 99, "started": __import__("time").time(),
                    "attempts": 0, "fingerprint": None})))

    def test_clean_success_revalidates_and_merges_exact_sha_then_closes(self):
        merged = dict(pr(state="closed"), merged=True, merge_commit_sha="c" * 40)
        calls = []

        def fake_api(endpoint, method="GET", body=None):
            calls.append((endpoint, method, body))

        p1, p2, p3, p4, p5 = self.base_patches()
        with p1, p2, p3, p4, p5, patch.object(review, "check_state", return_value=("success", [], [])), \
                patch.object(review, "api", side_effect=fake_api), \
                patch.object(review, "one", return_value=merged), patch.object(review, "record"):
            self.assertEqual(review.run(ROOT, 28, 99), "c" * 40)
        merge = next(call for call in calls if call[0].endswith("/merge"))
        self.assertEqual(merge[2], {"merge_method": "squash", "sha": "a" * 40})
        self.assertIn((f"repos/{review.REPO}/issues/28", "PATCH", {"state": "closed"}), calls)

    def test_new_dependency_during_final_revalidation_never_merges(self):
        p1, _, p3, p4, p5 = self.base_patches()
        with p1, p3, p4, p5, patch.object(review, "check_state", return_value=("success", [], [])), \
                patch.object(review, "open_dependencies", side_effect=[[], [26]]), \
                patch.object(review, "transition_issue", side_effect=review.ReviewStopped("dependency")), \
                patch.object(review, "api") as api:
            with self.assertRaises(review.ReviewStopped):
                review.run(ROOT, 28, 99)
            self.assertFalse(any(call.args[0].endswith("/merge") for call in api.call_args_list))

    def test_repeated_failure_stops_without_second_remediation(self):
        failed = [{"name": "Linux", "output": {"summary": "same"}}]
        p1, p2, p3, p4, p5 = self.base_patches()
        with p1, p2, p3, p4, p5, patch.object(review, "check_state", return_value=("failed", failed, [])), \
                patch.object(review, "git", side_effect=["a" * 40, "symphony/issue-28",
                                                         "b" * 40, "", "b" * 40, "",
                                                         "fixture change"]), \
                patch.object(review, "remediation") as remediate, \
                patch.object(review, "local_verify"), patch("subprocess.run"), \
                patch.object(review, "push"), \
                patch.object(review, "record"), patch.object(review, "save_state"), \
                patch.object(review, "transition_issue", side_effect=review.ReviewStopped("repeat")):
            with self.assertRaisesRegex(review.ReviewStopped, "repeat"):
                review.run(ROOT, 28, 99)
            self.assertEqual(remediate.call_count, 1)

    def test_attempt_is_persisted_before_remediation_push(self):
        failed = [{"name": "Linux", "output": {"summary": "new failure"}}]
        order = []
        p1, p2, p3, p4, p5 = self.base_patches()
        with p1, p2, p3, p4, p5, \
                patch.object(review, "check_state", return_value=("failed", failed, [])), \
                patch.object(review, "git", side_effect=["a" * 40, "symphony/issue-28",
                                                         "b" * 40, "", "b" * 40, "",
                                                         "fixture change"]), \
                patch.object(review, "remediation"), patch.object(review, "local_verify"), \
                patch.object(review, "save_state",
                             side_effect=lambda _path, state: order.append(
                                 ("save", state["attempts"], state["fingerprint"]))), \
                patch.object(review, "push",
                             side_effect=lambda *_args: (_ for _ in ()).throw(
                                 review.ReviewStopped("push failed"))):
            with self.assertRaisesRegex(review.ReviewStopped, "push failed"):
                review.run(ROOT, 28, 99)
        self.assertEqual(order[0][0:2], ("save", 1))
        self.assertRegex(order[0][2], r"^[0-9a-f]{64}$")


if __name__ == "__main__":
    unittest.main()

"""Behavior tests for the trusted post-publication state machine."""

import importlib.util
import json
import os
from pathlib import Path
import sys
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
            "base": {"ref": "main", "repo": {"full_name": review.REPO}},
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
        success = {"name": "Linux", "status": "completed", "conclusion": "success"}
        with patch.dict(os.environ, {"SYMPHONY_REQUIRED_CHECKS": "Linux,macOS"}), \
                patch.object(review, "one", return_value={"check_runs": [success]}):
            self.assertEqual(review.check_state("a" * 40)[0], "pending")
        failed = {"name": "macOS", "status": "completed", "conclusion": "failure",
                  "output": {"title": "test failed", "summary": "bounded diagnostic"}}
        with patch.dict(os.environ, {"SYMPHONY_REQUIRED_CHECKS": "Linux,macOS"}), \
                patch.object(review, "one", return_value={"check_runs": [success, failed]}):
            state, failures, _ = review.check_state("a" * 40)
            self.assertEqual(state, "failed")
            self.assertEqual(review.failed_logs(failures)[0]["check"], "macOS")

    def test_context_rejects_credentials_before_codex(self):
        context = {"review": "github_pat_" + "x" * 30}
        with patch("subprocess.run") as run:
            with self.assertRaises(review.ReviewStopped):
                review.remediation(ROOT, context)
            run.assert_not_called()

    def test_unresolved_coderabbit_line_comment_keeps_location_and_sha(self):
        response = {"data": {"repository": {"pullRequest": {"reviewThreads": {
            "pageInfo": {"hasNextPage": False}, "nodes": [{"isResolved": False,
                "comments": {"pageInfo": {"hasNextPage": False}, "nodes": [{
                    "body": "actionable finding", "path": "src/lib.rs", "line": 7,
                    "commit": {"oid": "a" * 40}, "author": {"login": "coderabbitai[bot]"}}]}}]}}}}}
        completed = __import__("subprocess").CompletedProcess([], 0, json.dumps(response), "")
        with patch.dict(os.environ, {"SYMPHONY_GITHUB_TOKEN": "test-token"}), \
                patch("subprocess.run", return_value=completed):
            self.assertEqual(review.review_threads(99), [{"body": "actionable finding",
                "path": "src/lib.rs", "line": 7, "commit_sha": "a" * 40}])


class StateMachineTests(unittest.TestCase):
    def base_patches(self):
        return (patch.object(review, "expected_pr", return_value=pr()),
                patch.object(review, "open_dependencies", return_value=[]),
                patch.object(review, "risk_reason", return_value=None),
                patch.object(review, "review_threads", return_value=[]),
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
                                                         "b" * 40, "", "b" * 40,
                                                         "symphony/issue-28"]), \
                patch.object(review, "remediation") as remediate, \
                patch.object(review, "local_verify"), patch("subprocess.run"), \
                patch.object(review, "record"), patch.object(review, "save_state"), \
                patch.object(review, "transition_issue", side_effect=review.ReviewStopped("repeat")):
            with self.assertRaisesRegex(review.ReviewStopped, "repeat"):
                review.run(ROOT, 28, 99)
            self.assertEqual(remediate.call_count, 1)


if __name__ == "__main__":
    unittest.main()

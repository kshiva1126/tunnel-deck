#!/usr/bin/env python3
"""Trusted hook dispatch; no workspace code owns admission state."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys

REPO = "kshiva1126/tunnel-deck"
API_TIMEOUT = 30


class Refused(Exception):
    pass


def api(endpoint, method="GET", body=None):
    env = os.environ.copy()
    token = env.get("SYMPHONY_GITHUB_TOKEN")
    if not token:
        raise Refused("GitHub credential unavailable")
    env["GH_TOKEN"] = token
    env.pop("GH_DEBUG", None)
    args = ["gh", "api", "--hostname", "github.com", "--method", method,
            "-H", "Accept: application/vnd.github+json", endpoint]
    if method == "GET":
        args += ["--paginate", "--slurp"]
    if body is not None:
        args += ["--input", "-"]
    try:
        result = subprocess.run(args, env=env, input=json.dumps(body) if body else None,
                                stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                text=True, timeout=API_TIMEOUT, check=True)
        if method != "GET":
            return None
        pages = json.loads(result.stdout)
        if not isinstance(pages, list) or not pages:
            raise ValueError()
        return pages
    except (OSError, subprocess.SubprocessError, ValueError):
        # Never include CLI stderr, response text, or environment values.
        raise Refused("GitHub API failed, timed out, or returned invalid data") from None


def listing(endpoint):
    pages = api(endpoint)
    if any(not isinstance(page, list) for page in pages):
        raise Refused("GitHub API returned an invalid list")
    return [item for page in pages for item in page]


def check(number):
    pages = api(f"repos/{REPO}/issues/{number}")
    if len(pages) != 1 or not isinstance(pages[0], dict):
        raise Refused("GitHub API returned an invalid issue")
    issue = pages[0]
    labels = issue.get("labels")
    if (type(issue.get("number")) is not int or issue["number"] != number
            or issue.get("state") not in ("open", "closed")
            or "pull_request" in issue or not isinstance(labels, list)
            or any(not isinstance(label, dict) or not isinstance(label.get("name"), str)
                   for label in labels)):
        raise Refused("GitHub API returned an invalid issue")
    names = {label["name"] for label in labels}
    if issue["state"] != "open" or "human-review" in names:
        raise Refused("issue already closed or awaiting human review")
    if "agent-ready" not in names or "blocked" in names:
        raise Refused("issue is not eligible (agent-ready required; blocked must be removed)")

    dependencies = listing(f"repos/{REPO}/issues/{number}/dependencies/blocked_by?per_page=100")
    blockers = []
    for dependency in dependencies:
        if (not isinstance(dependency, dict)
                or dependency.get("state") not in ("open", "closed")
                or type(dependency.get("number")) is not int
                or dependency["number"] <= 0
                or not isinstance(dependency.get("repository_url"), str)
                or not re.fullmatch(r"https://api\.github\.com/repos/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+",
                                    dependency["repository_url"])):
            raise Refused("GitHub API returned an invalid dependency")
        if dependency["state"] == "open":
            slug = dependency["repository_url"].removeprefix("https://api.github.com/repos/")
            blockers.append(f"{slug}#{dependency['number']}")
    if blockers:
        raise Refused("open dependencies: " + ", ".join(blockers))

    # All states, including merged/closed PRs, and all pages. Never reuse a
    # previously published branch automatically, even if its issue stayed open.
    prs = listing(f"repos/{REPO}/pulls?state=all&head=kshiva1126:symphony/issue-{number}&per_page=100")
    if prs:
        raise Refused("branch already has PR history; human review required")


def state_root():
    root = Path(os.environ["SYMPHONY_STATE_ROOT"])
    if not root.is_absolute():
        raise Refused("admission state path must be absolute")
    root.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = root.lstat()
    if root.is_symlink() or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise Refused("admission state must be owned by the hook user with mode 0700")
    return root


def stop(root, number, reason):
    # Persist first: API/label failure must never turn a retry into admission.
    (root / f"GH-{number}.stopped").write_text(reason + "\n")
    print(f"symphony gate: GH-{number}: {reason}", file=sys.stderr)
    try:
        api(f"repos/{REPO}/issues/{number}/labels/agent-ready", "DELETE")
    except Refused:
        (root / "halt").write_text("Cannot remove agent-ready; repair GitHub access before restarting.\n")
        print("symphony gate: label removal failed; worker shutdown requested", file=sys.stderr)
        return
    try:
        api(f"repos/{REPO}/issues/{number}/labels", "POST", {"labels": ["blocked"]})
    except Refused:
        print("symphony gate: agent-ready removed; blocked label could not be added", file=sys.stderr)


def main():
    os.umask(0o077)
    mode, workspace = sys.argv[1:]
    match = re.fullmatch(r"GH-([1-9][0-9]*)", Path(workspace).name)
    if not match or mode not in ("before", "after", "verify"):
        raise Refused("invalid hook mode or workspace name")
    number = int(match[1])
    if mode == "verify":
        check(number)
        return 0
    root = state_root()
    permit = root / f"GH-{number}.permit"
    stopped = root / f"GH-{number}.stopped"
    if mode == "after":
        if not permit.exists():
            return 0  # Symphony invokes after_run even when before_run failed.
        permit.unlink()
    else:
        permit.unlink(missing_ok=True)
    if stopped.exists():
        # A stale tracker snapshot or manual relabel can dispatch a stopped
        # issue again. Failing the hook alone lets Symphony retry forever.
        # Stop the supervisor without repeating GitHub writes or comments.
        (root / "halt").write_text(
            f"GH-{number} was dispatched while stopped; operator recovery required.\n")
        raise Refused(f"GH-{number} is stopped; worker shutdown requested")
    if (root / "halt").exists():
        raise Refused(f"GH-{number} is stopped; operator recovery required")
    try:
        check(number)
        hook = Path(os.environ["SYMPHONY_CONTROL_ROOT"]) / "scripts/symphony" / f"{mode}_run.sh"
        try:
            result = subprocess.run([str(hook), workspace], check=False)
        except OSError:
            # A missing/non-executable trusted hook must latch just like a
            # nonzero exit, rather than leaving Symphony free to retry it.
            raise Refused(f"{mode}_run hook could not start; inspect trusted installation") from None
        if result.returncode:
            raise Refused(f"{mode}_run hook failed; inspect hook diagnostics")
        if mode == "before":
            permit.touch(mode=0o600)
        else:
            # Protect against lost label updates / stale tracker snapshots.
            stopped.write_text("published; human review required\n")
        return 0
    except Refused as error:
        stop(root, number, str(error))
        return 1


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Refused as error:
        print(f"symphony gate: {error}", file=sys.stderr)
        sys.exit(1)
    except (OSError, KeyError):
        print("symphony gate: admission refused; inspect trusted state and configuration", file=sys.stderr)
        sys.exit(1)

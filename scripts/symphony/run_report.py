#!/usr/bin/env python3
"""Validate a Codex run report and upsert its bounded Issue comment."""

import json
import html
import os
from pathlib import Path
import re
import subprocess
import sys

REPO = "kshiva1126/tunnel-deck"
REPORT_NAME = ".symphony-run-report.json"
MAX_REPORT_BYTES = 48 * 1024
MAX_COMMENT_BYTES = 60 * 1024
API_TIMEOUT = 30
STATUSES = {"in_progress", "completed", "failed", "blocked", "interrupted", "publish_failed"}
MARKER = "<!-- tunnel-deck-symphony-run-report -->"
CREDENTIAL = re.compile(
    r"(?i)(github_pat_[A-Za-z0-9_]{20,}|gh[opsur]_[A-Za-z0-9_]{20,}|"
    r"sk-[A-Za-z0-9_-]{20,}|-----BEGIN [A-Z ]*PRIVATE KEY-----|"
    r"(?:access|refresh|id)[_-]?token\s*[:=]\s*\S+)"
)


class ReportError(Exception):
    pass


def report_path(workspace):
    return Path(workspace) / REPORT_NAME


def initial_report(number, run_id):
    return {
        "schema_version": 1,
        "issue_number": number,
        "run_id": run_id,
        "status": "interrupted",
        "scope": "Codex turn started; implementation summary was not completed.",
        "facts": [],
        "decisions": [],
        "alternatives": [],
        "execution": {
            "entry_points": [], "state_owner": "trusted Symphony hooks",
            "external_effects": [], "failure_cleanup": []
        },
        "tests": [],
        "unknowns": ["The Codex turn may have ended before recording its findings."],
        "commit": None,
        "pull_request": None,
    }


def write_initial(workspace, number, run_id):
    path = report_path(workspace)
    path.write_text(json.dumps(initial_report(number, run_id), indent=2) + "\n")
    path.chmod(0o600)


def load_report(workspace, expected_number):
    path = report_path(workspace)
    try:
        raw = path.read_bytes()
    except OSError:
        raise ReportError("run report is missing") from None
    if not raw or len(raw) > MAX_REPORT_BYTES:
        raise ReportError("run report is empty or oversized")
    try:
        report = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError):
        raise ReportError("run report is not valid JSON") from None
    required = {"schema_version", "issue_number", "run_id", "status", "scope", "facts",
                "decisions", "alternatives", "execution", "tests", "unknowns", "commit",
                "pull_request"}
    if not isinstance(report, dict) or set(report) != required:
        raise ReportError("run report fields are invalid")
    if (report["schema_version"] != 1 or type(report["issue_number"]) is not int
            or report["issue_number"] != expected_number
            or not isinstance(report["run_id"], str)
            or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,79}", report["run_id"])
            or not isinstance(report["status"], str) or report["status"] not in STATUSES
            or not isinstance(report["scope"], str)):
        raise ReportError("run report identity or status is invalid")
    for key in ("facts", "decisions", "alternatives", "tests", "unknowns"):
        if (not isinstance(report[key], list)
                or any(not isinstance(item, (str, dict)) for item in report[key])):
            raise ReportError(f"run report {key} is invalid")
    execution = report["execution"]
    if (not isinstance(execution, dict)
            or set(execution) != {"entry_points", "state_owner", "external_effects", "failure_cleanup"}
            or not isinstance(execution["state_owner"], str)
            or any(not isinstance(execution[k], list)
                   for k in ("entry_points", "external_effects", "failure_cleanup"))):
        raise ReportError("run report execution is invalid")
    if any(not isinstance(item, str) for key in
           ("entry_points", "external_effects", "failure_cleanup") for item in execution[key]):
        raise ReportError("run report execution entry is invalid")
    if report["commit"] is not None and (not isinstance(report["commit"], str)
            or not re.fullmatch(r"[0-9a-f]{7,40}", report["commit"])):
        raise ReportError("run report commit is invalid")
    if report["pull_request"] is not None and (not isinstance(report["pull_request"], str)
            or not re.fullmatch(
                r"https://github\.com/kshiva1126/tunnel-deck/pull/[1-9][0-9]*",
                report["pull_request"])):
        raise ReportError("run report pull request is invalid")
    if CREDENTIAL.search(raw.decode("utf-8")):
        raise ReportError("run report contains credential-like content")
    return report


def bullets(values):
    if not values:
        return "- なし"
    rendered = []
    for value in values:
        if isinstance(value, str):
            text = value
        elif isinstance(value, dict):
            if any(not isinstance(k, str) or not isinstance(v, str) for k, v in value.items()):
                raise ReportError("run report mapping entry is invalid")
            text = "; ".join(f"{k}: {v}" for k, v in value.items())
        else:
            raise ReportError("run report list entry is invalid")
        rendered.append("- " + safe_text(text).replace("\n", " "))
    return "\n".join(rendered)


def safe_text(value):
    """Escape report-controlled HTML while preserving Markdown and newlines."""
    return html.escape(value, quote=False)


def render_attempt(report):
    execution = report["execution"]
    entry_points = ", ".join(safe_text(item) for item in execution["entry_points"])
    external_effects = ", ".join(safe_text(item) for item in execution["external_effects"])
    failure_cleanup = ", ".join(safe_text(item) for item in execution["failure_cleanup"])
    links = []
    if report["commit"]:
        links.append(f"commit: [`{report['commit']}`](https://github.com/{REPO}/commit/{report['commit']})")
    if report["pull_request"]:
        links.append(f"PR: {report['pull_request']}")
    return f"""<!-- tunnel-deck-symphony-run:{report['run_id']} -->
### 試行 `{report['run_id']}` — `{report['status']}`

#### 課題理解とスコープ

{safe_text(report['scope'])}

#### 判明した事実

{bullets(report['facts'])}

#### 採用した判断と理由

{bullets(report['decisions'])}

#### 主要な代替案と不採用理由

{bullets(report['alternatives'])}

#### 実行経路と後始末

- 入口: {entry_points or 'なし'}
- 状態所有者: {safe_text(execution['state_owner'])}
- 外部作用: {external_effects or 'なし'}
- 失敗時の後始末: {failure_cleanup or 'なし'}

#### 検証

{bullets(report['tests'])}

#### 未確認事項・リスク・owner判断

{bullets(report['unknowns'])}

#### 関連リンク

{bullets(links)}
<!-- /tunnel-deck-symphony-run:{report['run_id']} -->"""


def gh_api(endpoint, method="GET", body=None, paginate=True):
    token = os.environ.get("SYMPHONY_GITHUB_TOKEN")
    if not token:
        raise ReportError("GitHub credential unavailable")
    env = os.environ.copy()
    env["GH_TOKEN"] = token
    env.pop("GH_DEBUG", None)
    args = ["gh", "api", "--hostname", "github.com", "--method", method,
            "-H", "Accept: application/vnd.github+json", endpoint]
    if method == "GET" and paginate:
        args += ["--paginate", "--slurp"]
    if body is not None:
        args += ["--input", "-"]
    try:
        result = subprocess.run(args, env=env, input=json.dumps(body) if body is not None else None,
                                text=True, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                timeout=API_TIMEOUT, check=True)
        return json.loads(result.stdout) if result.stdout else None
    except (OSError, subprocess.SubprocessError, json.JSONDecodeError):
        raise ReportError("GitHub comment API failed or timed out") from None


def upsert(report):
    number = report["issue_number"]
    actor = gh_api("user", paginate=False)
    if (not isinstance(actor, dict) or not isinstance(actor.get("login"), str)
            or not actor["login"]):
        raise ReportError("authenticated GitHub actor is invalid")
    pages = gh_api(f"repos/{REPO}/issues/{number}/comments?per_page=100")
    if not isinstance(pages, list) or any(not isinstance(page, list) for page in pages):
        raise ReportError("GitHub comments response is invalid")
    matches = [item for page in pages for item in page
               if isinstance(item, dict) and isinstance(item.get("body"), str)
               and isinstance(item.get("user"), dict)
               and item["user"].get("login") == actor["login"]
               and item["body"].startswith(MARKER)]
    if len(matches) > 1:
        raise ReportError("multiple Symphony report comments found")
    attempt = render_attempt(report)
    start = f"<!-- tunnel-deck-symphony-run:{report['run_id']} -->"
    end = f"<!-- /tunnel-deck-symphony-run:{report['run_id']} -->"
    if matches:
        existing = matches[0]
        if type(existing.get("id")) is not int or existing["id"] <= 0:
            raise ReportError("GitHub report comment is invalid")
        body = existing["body"]
        if start in body:
            before, remainder = body.split(start, 1)
            if end not in remainder:
                raise ReportError("GitHub report comment is malformed")
            body = before + attempt + remainder.split(end, 1)[1]
        else:
            body += "\n\n" + attempt
        endpoint = f"repos/{REPO}/issues/comments/{existing['id']}"
        method = "PATCH"
        comment_id = existing["id"]
    else:
        body = MARKER + "\n## Symphony 実装判断・検証レポート\n\n" + attempt
        endpoint = f"repos/{REPO}/issues/{number}/comments"
        method = "POST"
        comment_id = None
    if len(body.encode()) > MAX_COMMENT_BYTES or CREDENTIAL.search(body):
        raise ReportError("rendered comment is oversized or contains credential-like content")
    response = gh_api(endpoint, method, {"body": body})
    if isinstance(response, dict) and type(response.get("id")) is int:
        comment_id = response["id"]
    if comment_id is None:
        raise ReportError("GitHub comment response is invalid")
    return f"https://github.com/{REPO}/issues/{number}#issuecomment-{comment_id}"


def main():
    command, workspace, number_text, *rest = sys.argv[1:]
    if not re.fullmatch(r"[1-9][0-9]*", number_text):
        raise ReportError("issue number is invalid")
    number = int(number_text)
    if command == "init" and len(rest) == 1:
        write_initial(workspace, number, rest[0])
        return
    if command == "publish" and not rest:
        print(upsert(load_report(workspace, number)))
        return
    if command == "validate-complete" and not rest:
        if load_report(workspace, number)["status"] != "completed":
            raise ReportError("committed run report is not completed")
        return
    raise ReportError("run report command is invalid")


if __name__ == "__main__":
    try:
        main()
    except (ReportError, OSError, ValueError) as error:
        print(f"symphony report: {error}", file=sys.stderr)
        sys.exit(1)

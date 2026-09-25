#!/usr/bin/env python3

import argparse
import json
import math
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path, PurePosixPath

MAX_FILE_BYTES = 2_000_000
TEMPLATE_FILE = re.compile(r"(?:\.example|\.sample|\.template)$", re.IGNORECASE)
SECRET_FILE_PATTERNS = (
    re.compile(r"^\.env(?:\..*)?$", re.IGNORECASE),
    re.compile(r"^\.envrc$", re.IGNORECASE),
    re.compile(r"^(?:\.netrc|\.npmrc|\.pypirc)$", re.IGNORECASE),
    re.compile(r"^id_(?:rsa|dsa|ecdsa|ed25519)$", re.IGNORECASE),
    re.compile(r"^credentials(?:\..*)?\.json$", re.IGNORECASE),
    re.compile(r"^secrets?\.(?:json|ya?ml|toml)$", re.IGNORECASE),
    re.compile(r"^service[-_]?account.*\.json$", re.IGNORECASE),
    re.compile(r".*\.(?:pem|key|p12|pfx|jks|keystore|kdbx)$", re.IGNORECASE),
)
SECRET_VALUE_PATTERNS = (
    ("private-key", re.compile(r"-----BEGIN (?:[A-Z0-9]+ )?PRIVATE KEY-----")),
    (
        "github-token",
        re.compile(
            r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})(?![A-Za-z0-9_])"
        ),
    ),
    ("gitlab-token", re.compile(r"\bglpat-[A-Za-z0-9_-]{20,}(?![A-Za-z0-9_-])")),
    (
        "anthropic-token",
        re.compile(r"\bsk-ant-[A-Za-z0-9_-]{20,}(?![A-Za-z0-9_-])"),
    ),
    (
        "openrouter-token",
        re.compile(r"\bsk-or-v1-[A-Za-z0-9]{20,}(?![A-Za-z0-9])"),
    ),
    (
        "openai-token",
        re.compile(r"\bsk-(?!ant-|or-v1-)(?:proj-)?[A-Za-z0-9_-]{20,}(?![A-Za-z0-9_-])"),
    ),
    ("aws-access-key", re.compile(r"\b(?:AKIA|ASIA)[A-Z0-9]{16}(?![A-Z0-9])")),
    ("google-api-key", re.compile(r"\bAIza[A-Za-z0-9_-]{30,}(?![A-Za-z0-9_-])")),
    ("slack-token", re.compile(r"\bxox[baprs]-[A-Za-z0-9-]{20,}(?![A-Za-z0-9-])")),
    ("stripe-live-key", re.compile(r"\b(?:sk|rk)_live_[A-Za-z0-9]{20,}(?![A-Za-z0-9])")),
    ("npm-token", re.compile(r"\bnpm_[A-Za-z0-9]{30,}(?![A-Za-z0-9])")),
    ("huggingface-token", re.compile(r"\bhf_[A-Za-z0-9]{30,}(?![A-Za-z0-9])")),
    (
        "pypi-token",
        re.compile(r"\bpypi-[A-Za-z0-9_-]{40,}(?![A-Za-z0-9_-])"),
    ),
    (
        "telegram-token",
        re.compile(r"\b[0-9]{8,10}:[A-Za-z0-9_-]{35}(?![A-Za-z0-9_-])"),
    ),
    (
        "jwt",
        re.compile(
            r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}"
            r"(?![A-Za-z0-9_-])"
        ),
    ),
)
SECRET_NAME = r"[A-Za-z_][A-Za-z0-9_-]*(?:KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIALS?)[A-Za-z0-9_-]*"
ASSIGNMENT = re.compile(
    rf"^\s*(?:export\s+)?(?P<name>{SECRET_NAME})\s*=\s*"
    r"(?:\"(?P<double>[^\"]+)\"|'(?P<single>[^']+)'|(?P<bare>[^\s#]+))",
    re.IGNORECASE,
)
JSON_ASSIGNMENT = re.compile(
    rf"^\s*\"(?P<name>{SECRET_NAME})\"\s*:\s*"
    r"(?:\"(?P<double>[^\"]+)\"|'(?P<single>[^']+)'|(?P<bare>[^\s#]+))",
    re.IGNORECASE,
)
YAML_ASSIGNMENT = re.compile(
    rf"^\s*(?P<name>{SECRET_NAME})\s*:\s*"
    r"(?:\"(?P<double>[^\"]+)\"|'(?P<single>[^']+)'|(?P<bare>[^\s#]+))",
    re.IGNORECASE,
)
RUST_CONST_ASSIGNMENT = re.compile(
    rf"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const|static)\s+"
    rf"(?P<name>{SECRET_NAME})\s*:\s*&str\s*=\s*"
    r"(?:\"(?P<double>[^\"]+)\"|'(?P<single>[^']+)'|(?P<bare>[^\s#]+))",
    re.IGNORECASE,
)
CONFIG_SUFFIXES = {
    ".cfg",
    ".conf",
    ".env",
    ".ini",
    ".json",
    ".jsonc",
    ".properties",
    ".toml",
    ".yaml",
    ".yml",
}
SHELL_FILENAMES = {".bash", ".sh", ".zsh", "Dockerfile"}
PLACEHOLDER_MARKERS = (
    "example",
    "placeholder",
    "changeme",
    "dummy",
    "fake",
    "not-a-real",
    "redacted",
    "replace_me",
    "test-key",
    "test_key",
    "test-token",
    "test_token",
    "your-key",
    "your_key",
    "your-token",
    "your_token",
    "xxxxx",
)


class GitError(RuntimeError):
    pass


def git(repo: Path, *args: str) -> bytes:
    result = subprocess.run(
        ["git", *args],
        cwd=repo,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        raise GitError("git command failed")
    return result.stdout


def placeholder(value: str) -> bool:
    normalized = value.strip().strip("\"'` \t\r\n").lower()
    if not normalized or normalized in {"0", "1", "false", "none", "null", "true"}:
        return True
    if normalized.startswith(("${", "{{", "<")):
        return True
    return any(marker in normalized for marker in PLACEHOLDER_MARKERS)


def shannon_entropy(value: str) -> float:
    if not value:
        return 0.0
    return -sum(
        (count / len(value)) * math.log2(count / len(value))
        for count in Counter(value).values()
    )


def suspicious_assignment(value: str) -> bool:
    return len(value) >= 12 and not placeholder(value) and shannon_entropy(value) >= 3.5


def assignment_value(assignment: re.Match[str]) -> str:
    return next(
        candidate
        for candidate in assignment.group("double", "single", "bare")
        if candidate is not None
    )


def config_path(path: str) -> bool:
    file = PurePosixPath(path)
    name = file.name
    if name == ".env" or name.startswith(".env.") or name == ".envrc":
        return True
    return file.suffix.lower() in CONFIG_SUFFIXES or name in SHELL_FILENAMES


def secret_filename(name: str) -> bool:
    if TEMPLATE_FILE.search(name):
        return False
    return any(pattern.fullmatch(name) for pattern in SECRET_FILE_PATTERNS)


def scan(repo: Path) -> tuple[list[tuple[str, int, str]], int]:
    paths = git(repo, "ls-files", "-z").decode("utf-8", "surrogateescape").split("\0")
    findings: set[tuple[str, int, str]] = set()
    scanned = 0
    for path in filter(None, paths):
        name = PurePosixPath(path).name
        if secret_filename(name):
            findings.add((path, 0, "secret-filename"))
        size_result = subprocess.run(
            ["git", "cat-file", "-s", f":{path}"],
            cwd=repo,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if size_result.returncode != 0:
            continue
        try:
            size = int(size_result.stdout)
        except ValueError:
            continue
        if size > MAX_FILE_BYTES:
            continue
        data = git(repo, "cat-file", "blob", f":{path}")
        try:
            text = data.decode("utf-8")
        except UnicodeDecodeError:
            continue
        scanned += 1
        for line_number, line in enumerate(text.splitlines(), 1):
            for rule, pattern in SECRET_VALUE_PATTERNS:
                match = pattern.search(line)
                if match and not placeholder(match.group(0)):
                    findings.add((path, line_number, rule))
            assignments = []
            if config_path(path):
                assignments.extend((ASSIGNMENT, JSON_ASSIGNMENT, YAML_ASSIGNMENT))
            if PurePosixPath(path).suffix.lower() == ".rs":
                assignments.append(RUST_CONST_ASSIGNMENT)
            for assignment_pattern in assignments:
                assignment = assignment_pattern.match(line)
                if assignment is not None and suspicious_assignment(assignment_value(assignment)):
                    findings.add((path, line_number, "secret-assignment"))
    return sorted(findings), scanned


def finding_location(path: str, line: int) -> str:
    safe_path = json.dumps(path, ensure_ascii=True)
    return safe_path if line == 0 else f"{safe_path}:{line}"


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Scan tracked files for likely secrets without printing matched values."
    )
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    args = parser.parse_args()
    try:
        repo = args.repo.resolve()
        findings, scanned = scan(repo)
    except (GitError, OSError):
        print("secret scan failed", file=sys.stderr)
        return 2
    if not findings:
        print(f"No likely secrets found in {scanned} tracked text files.")
        return 0
    print(f"Found {len(findings)} likely secret location(s) in {scanned} tracked text files:")
    for path, line, rule in findings:
        print(f"{finding_location(path, line)}: {rule}")
    return 1


if __name__ == "__main__":
    sys.exit(main())

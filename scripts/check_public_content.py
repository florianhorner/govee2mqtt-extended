#!/usr/bin/env python3
"""Public-content guard for a public repository.

Scans tracked (or staged) text files for content that must not be published:
private network addresses, device identifiers, home-directory paths, personal
e-mail addresses, and internal review or tooling narration. Also refuses a
tracked engineering backlog (todos.md), which lives outside the repository.

Usage:
  scripts/check_public_content.py --all          # every tracked file (CI)
  scripts/check_public_content.py --staged       # staged blobs (pre-commit)
  scripts/check_public_content.py PATH [PATH...] # explicit files

Exit codes: 0 clean (warnings allowed), 1 blocking hit, 2 usage or git error.

Every blocking rule was calibrated against the whole tracked tree before it was
added: it has zero false positives there, or its false positives are listed by
exact value below. Path exemptions are deliberately short. Fixture identifiers
are exempted by value, not by directory, so a real identifier pasted into a
test or a snapshot still fails.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

SELF = Path(__file__).name
# These three files must contain the very strings the rules look for. They are
# the one place a leak could be hidden from this guard, so review their diffs.
NEVER_SCANNED = {
    f"scripts/{SELF}",
    "scripts/test_check_public_content.py",
    "scripts/lint-patterns.sh",
}
# Only files with these extensions are treated as binary. A text file that
# merely contains a NUL byte is still scanned, so a NUL cannot hide a leak.
BINARY_SUFFIXES = {".png", ".jpg", ".jpeg", ".gif", ".ico", ".webp", ".woff", ".woff2",
                   ".ttf", ".pdf", ".zip", ".gz", ".tgz", ".tar", ".bin", ".so", ".dylib"}

# Upstream wez/govee2mqtt issue fixtures carry these ids; they are sample data.
UPSTREAM_FIXTURE_DEVICE_IDS = {
    "02:EC:CF:00:00:00:00:48",
    "47:13:CF:00:00:00:00:25",
    "51:2A:D1:00:00:00:00:93",
    "52:8B:D4:AD:FC:45:5D:FE",
    "69:EC:D1:37:36:39:24:4B",
    "9D:FA:85:EB:D3:00:8B:FF",
    "B6:21:C3:37:34:32:33:86",
}
FIXTURE_MACS = {"CF:00:00:00:00:25", "CF:00:00:00:00:48", "D1:00:00:00:00:93"}
FIXTURE_LAN_IPS = {"192.168.1.50", "192.168.1.51"}
# Placeholder domains used by fixtures and tests: any local part is fine there.
PLACEHOLDER_EMAIL_DOMAINS = {"example.com", "b.com", "x.com"}
# Real domains are allowed only for these exact vendor and service addresses.
ALLOWED_EMAIL_ADDRESSES = {"no-reply@govee.com", "support@govee.com", "git@github.com"}


def _placeholder_id(value: str) -> bool:
    return "AA:BB:CC" in value.upper()


def _placeholder_compact(value: str) -> bool:
    return value.upper().startswith("AABBCC") or value.isdigit()


def _example_email(value: str) -> bool:
    lowered = value.lower()
    return (lowered.rsplit("@", 1)[-1] in PLACEHOLDER_EMAIL_DOMAINS
            or lowered in ALLOWED_EMAIL_ADDRESSES)


@dataclass(frozen=True)
class Rule:
    id: str
    pattern: str
    block: bool
    why: str
    ignore_case: bool = False
    exempt_paths: frozenset[str] = frozenset()
    exempt_value: object = None  # callable(str) -> bool
    exempt_values: frozenset[str] = frozenset()

    def regex(self) -> re.Pattern[str]:
        return re.compile(self.pattern, re.IGNORECASE if self.ignore_case else 0)


COMMIT_RULES = ".config/commit-rules.json"

RULES: list[Rule] = [
    # --- internal review and tooling narration ---------------------------------
    Rule("process-outside-voice", r"outside voice", True,
         "review-process vocabulary", ignore_case=True),
    Rule("process-adversarial-pass", r"adversarial[ -](review|pass|challenge)", True,
         "review-process vocabulary", ignore_case=True),
    Rule("process-pre-ship-landing", r"pre-(ship|landing)(?![A-Za-z0-9])", True,
         "internal release-process stage names", ignore_case=True),
    Rule("process-slash-commands",
         r"(^|[\s`(])/(ship|review|retro|autoplan|office-hours|codex|qa|investigate|plan-[a-z]+-review)(?![A-Za-z0-9_-])",
         True, "local tooling commands", exempt_paths=frozenset({COMMIT_RULES})),
    Rule("process-codex", r"codex", True, "review tool name", ignore_case=True,
         exempt_paths=frozenset({COMMIT_RULES, "AUTHOR-NOTES.md", "CLAUDE.md"})),
    Rule("process-gstack", r"gstack", True, "local tooling name", ignore_case=True,
         exempt_paths=frozenset({".gitignore", COMMIT_RULES})),
    Rule("process-subagent", r"subagent", True, "tooling internals", ignore_case=True,
         exempt_paths=frozenset({COMMIT_RULES})),
    Rule("process-status-tokens", r"DONE_WITH_CONCERNS|NEEDS_CONTEXT", True,
         "internal status vocabulary"),
    Rule("process-scope-discipline", r"scope discipline", True,
         "internal process vocabulary", ignore_case=True),
    Rule("process-review-ranking", r"(squad|ranked) P[0-3]", True,
         "internal review ranking"),
    Rule("process-maintainer-actor",
         r"Florian (sends|asked|approves|decides|clicks|opens|merges|reviews)", True,
         "maintainer as a process actor"),
    Rule("process-context-dir", r"\.context/", True, "agent scratch directory",
         exempt_paths=frozenset({".gitignore", "CONTRIBUTING.md"})),
    # --- local machine paths ---------------------------------------------------
    Rule("path-agent-dotdirs", r"\.(claude|gstack|codex|mempalace|conductor)/", True,
         "local tooling directories", exempt_paths=frozenset({".gitignore"})),
    Rule("path-users-home", r"/Users/[A-Za-z0-9_][A-Za-z0-9._-]*", True,
         "home-directory path", exempt_values=frozenset({"/Users/Shared"})),
    Rule("path-home-dir", r"/home/[A-Za-z0-9_][A-Za-z0-9._-]*", True,
         "home-directory path"),
    # --- private network -------------------------------------------------------
    Rule("net-192-168", r"192\.168\.[0-9]{1,3}\.[0-9]{1,3}", True,
         "private LAN address", exempt_values=frozenset(FIXTURE_LAN_IPS)),
    Rule("net-172-private", r"(?<![0-9.])172\.(1[6-9]|2[0-9]|3[01])\.[0-9]{1,3}\.[0-9]{1,3}",
         True, "private LAN address"),
    Rule("net-tailscale-cgnat",
         r"(?<![0-9.])100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.[0-9]{1,3}\.[0-9]{1,3}",
         True, "overlay-network address"),
    Rule("net-ipv6-private", r"(?<![0-9A-Fa-f:])f[cd][0-9A-Fa-f]{2}:[0-9A-Fa-f:]{2,}", True,
         "private IPv6 address (fc00::/7)"),
    Rule("net-private-hostnames", r"ha-green|\.ts\.net", True,
         "private host name", ignore_case=True),
    # --- device identifiers ----------------------------------------------------
    Rule("device-id-8-octet", r"([0-9A-Fa-f]{2}:){7}[0-9A-Fa-f]{2}", True,
         "Govee device id", exempt_value=_placeholder_id,
         exempt_values=frozenset(UPSTREAM_FIXTURE_DEVICE_IDS)),
    Rule("device-mac-6-octet",
         r"(?<![0-9A-Fa-f:])([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}(?![0-9A-Fa-f:])", True,
         "hardware address", exempt_value=_placeholder_id,
         exempt_values=frozenset(FIXTURE_MACS)),
    Rule("device-id-compact-hex16", r"(?<![A-Za-z0-9])[0-9A-F]{16}(?![A-Za-z0-9])", True,
         "compact Govee device id", exempt_value=_placeholder_compact),
    # --- people ----------------------------------------------------------------
    Rule("personal-email", r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}", True,
         "e-mail address", exempt_value=_example_email),
    # --- warnings: worth a look, not a failure ---------------------------------
    Rule("process-policy-actor", r"fork-safety policy|per policy|no agent opens", False,
         "policy narration", ignore_case=True),
    Rule("process-drive-by", r"drive-by", False, "review vocabulary", ignore_case=True),
    Rule("process-specialist-confidence",
         r"specialist reviewers?|specialist pass|classified FIXABLE|confidence:? [0-9]+([,)/]| of)",
         False, "review vocabulary", ignore_case=True),
    Rule("process-conductor", r"conductor", False, "workspace tooling", ignore_case=True,
         exempt_paths=frozenset({"CLAUDE.md", ".conductor/settings.toml", ".gitignore", COMMIT_RULES})),
    Rule("process-skill-word", r"(?<![A-Za-z0-9_])skills?(?![A-Za-z0-9_])", False,
         "tooling vocabulary", ignore_case=True,
         exempt_paths=frozenset({"CLAUDE.md", "CONTRIBUTING.md", COMMIT_RULES})),
    Rule("process-maintainer-name", r"(?<![/@A-Za-z0-9])Florian(?![A-Za-z0-9])", False,
         "maintainer named in prose"),
    Rule("net-10-slash-8", r"(?<![0-9.])10\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}", False,
         "possibly private address"),
    Rule("net-link-local", r"(?<![0-9.])169\.254\.[0-9]{1,3}\.[0-9]{1,3}", False,
         "link-local address"),
    Rule("net-generic-ha-hosts", r"tailscale|homeassistant\.local", False,
         "possibly private host", ignore_case=True),
    Rule("device-mac-dashed",
         r"(?<![0-9A-Fa-f-])([0-9A-Fa-f]{2}-){5}[0-9A-Fa-f]{2}(?![0-9A-Fa-f-])", False,
         "hardware address"),
]

TODOS_PATTERN = re.compile(r"(^|/)todos\.md$", re.IGNORECASE)


@dataclass
class Hit:
    path: str
    line: int
    rule: Rule
    value: str


@dataclass
class Report:
    blocks: list[Hit] = field(default_factory=list)
    warns: list[Hit] = field(default_factory=list)
    tracked_backlog: list[str] = field(default_factory=list)

    @property
    def failed(self) -> bool:
        return bool(self.blocks or self.tracked_backlog)


def git(*args: str, cwd: Path | None = None) -> str:
    result = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)}: {result.stderr.strip()}")
    return result.stdout


def tracked_files(root: Path) -> list[str]:
    return [p for p in git("ls-files", "-z", cwd=root).split("\0") if p]


def staged_files(root: Path) -> list[str]:
    out = git("diff", "--cached", "--name-only", "--diff-filter=ACMR", "-z", cwd=root)
    return [p for p in out.split("\0") if p]


def is_binary(path: str) -> bool:
    return Path(path).suffix.lower() in BINARY_SUFFIXES


def scan_text(path: str, text: str, rules: list[Rule] = RULES) -> list[Hit]:
    hits: list[Hit] = []
    compiled = [(rule, rule.regex()) for rule in rules if path not in rule.exempt_paths]
    for number, line in enumerate(text.splitlines(), start=1):
        for rule, regex in compiled:
            for match in regex.finditer(line):
                value = match.group(0)
                if value in rule.exempt_values:
                    continue
                if rule.exempt_value is not None and rule.exempt_value(value):
                    continue
                hits.append(Hit(path, number, rule, value))
    return hits


def check_paths(root: Path, paths: list[str], reader) -> Report:
    report = Report()
    for path in paths:
        if TODOS_PATTERN.search(path):
            report.tracked_backlog.append(path)
        if path in NEVER_SCANNED or is_binary(path):
            continue
        data = reader(path)
        if data is None:
            continue
        for hit in scan_text(path, data.decode("utf-8", errors="replace")):
            (report.blocks if hit.rule.block else report.warns).append(hit)
    return report


def read_worktree(root: Path):
    def reader(path: str) -> bytes | None:
        target = root / path
        if target.is_symlink():
            # Git stores the link target as the blob, so that text is what gets
            # published; scan it instead of skipping the entry.
            return str(target.readlink()).encode("utf-8")
        if not target.is_file():
            return None
        return target.read_bytes()
    return reader


def read_staged(root: Path):
    def reader(path: str) -> bytes | None:
        result = subprocess.run(["git", "show", f":{path}"], cwd=root, capture_output=True)
        return result.stdout if result.returncode == 0 else None
    return reader


def print_report(report: Report) -> None:
    for path in report.tracked_backlog:
        print(f"BLOCK {path}: the engineering backlog is private and must not be tracked "
              "(see CONTRIBUTING.md, Public content)")
    for hit in report.blocks:
        print(f"BLOCK {hit.path}:{hit.line}: [{hit.rule.id}] {hit.rule.why}: {hit.value!r}")
    if report.warns:
        by_rule: dict[str, list[Hit]] = {}
        for hit in report.warns:
            by_rule.setdefault(hit.rule.id, []).append(hit)
        for rule_id, hits in by_rule.items():
            places = ", ".join(f"{h.path}:{h.line}" for h in hits[:3])
            more = f" (+{len(hits) - 3} more)" if len(hits) > 3 else ""
            print(f"WARN  [{rule_id}] {len(hits)} hit(s): {places}{more}")
    if report.failed:
        print(f"public-content check FAILED: {len(report.blocks)} blocking hit(s), "
              f"{len(report.tracked_backlog)} tracked backlog file(s)")
    else:
        print(f"public-content check passed ({len(report.warns)} warning(s))")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--all", action="store_true", help="scan every tracked file")
    mode.add_argument("--staged", action="store_true", help="scan staged blobs")
    parser.add_argument("paths", nargs="*", help="explicit files, relative to the repo root")
    args = parser.parse_args(argv)
    try:
        root = Path(git("rev-parse", "--show-toplevel").strip())
        if args.staged:
            report = check_paths(root, staged_files(root), read_staged(root))
        elif args.all:
            report = check_paths(root, tracked_files(root), read_worktree(root))
        elif args.paths:
            missing = [p for p in args.paths if not (root / p).exists()]
            if missing:
                raise RuntimeError("no such file: " + ", ".join(missing))
            report = check_paths(root, args.paths, read_worktree(root))
        else:
            parser.error("give --all, --staged, or at least one path")
    except RuntimeError as err:
        print(f"ERROR: {err}", file=sys.stderr)
        return 2
    print_report(report)
    return 1 if report.failed else 0


if __name__ == "__main__":
    sys.exit(main())

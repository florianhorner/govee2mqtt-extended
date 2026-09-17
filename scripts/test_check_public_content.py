#!/usr/bin/env python3
"""Tests for scripts/check_public_content.py.

Run with: python3 -m unittest scripts/test_check_public_content.py
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import check_public_content as guard  # noqa: E402

GUARD = HERE / "check_public_content.py"


def blocked(text: str, path: str = "docs/example.md") -> list[str]:
    return sorted({hit.rule.id for hit in guard.scan_text(path, text) if hit.rule.block})


def warned(text: str, path: str = "docs/example.md") -> list[str]:
    return sorted({hit.rule.id for hit in guard.scan_text(path, text) if not hit.rule.block})


class RuleMatching(unittest.TestCase):
    def test_review_and_tooling_narration_is_blocked(self):
        cases = {
            "found by the outside voice": "process-outside-voice",
            "an adversarial review found it": "process-adversarial-pass",
            "during the pre-ship pass": "process-pre-ship-landing",
            "run /ship to land it": "process-slash-commands",
            "Codex flagged this": "process-codex",
            "wired through gstack": "process-gstack",
            "a coverage subagent": "process-subagent",
            "status: DONE_WITH_CONCERNS": "process-status-tokens",
            "scope discipline beats a fix": "process-scope-discipline",
            "raised as squad P1": "process-review-ranking",
            "and Florian sends it": "process-maintainer-actor",
            "drafted to .context/ first": "process-context-dir",
        }
        for text, rule in cases.items():
            with self.subTest(text=text):
                self.assertIn(rule, blocked(text))

    def test_private_data_is_blocked(self):
        cases = {
            "Compiling govee (/Users/someone/repo)": "path-users-home",
            "Compiling govee (/home/someone/repo)": "path-home-dir",
            "listening on fd12:3456:789a::1": "net-ipv6-private",
            "see ~/.claude/hooks for details": "path-agent-dotdirs",
            "LAN API: ip=192.168.178.40": "net-192-168",
            "bound to 172.16.4.2": "net-172-private",
            "reach it at 100.98.1.7": "net-tailscale-cgnat",
            "ssh ha-green": "net-private-hostnames",
            "device 30:F2:EA:56:E7:59:FB:1D": "device-id-8-octet",
            "mac 30:F2:EA:56:E7:59": "device-mac-6-octet",
            "topic gv2mqtt/30F2EA56E759FB1D/command": "device-id-compact-hex16",
            "mail me at someone@somewhere.org": "personal-email",
        }
        for text, rule in cases.items():
            with self.subTest(text=text):
                self.assertIn(rule, blocked(text))

    def test_fixture_and_placeholder_values_pass(self):
        clean = [
            "device AA:BB:CC:DD:EE:FF:00:11 in test-data",
            "device 52:8B:D4:AD:FC:45:5D:FE from an upstream issue",
            "mac CF:00:00:00:00:25",
            "topic gv2mqtt/AABBCCDDEEFF1122/command",
            "timestamp 1700000000000000",
            "unit test ip 192.168.1.50 and 192.168.1.51",
            "shared path /Users/Shared/data",
            "user@example.com and User@x.com and a@b.com",
            "no-reply@govee.com, support@govee.com and git@github.com",
        ]
        for text in clean:
            with self.subTest(text=text):
                self.assertEqual(blocked(text), [])

    def test_real_domains_are_exact_not_wildcard(self):
        self.assertIn("personal-email", blocked("alice@govee.com"))
        self.assertIn("personal-email", blocked("someone@github.com"))
        self.assertEqual(blocked("No-Reply@Govee.com"), [])

    def test_word_boundaries(self):
        self.assertEqual(blocked("pre-shipment inspection"), [])
        self.assertEqual(blocked("the /shipping route and /reviews page"), [])
        self.assertIn("process-slash-commands", blocked("then run `/review` again"))
        self.assertIn("process-pre-ship-landing", blocked("Pre-landing review"))

    def test_compact_id_needs_a_hex_letter(self):
        self.assertEqual(blocked("1234567890123456"), [])
        self.assertIn("device-id-compact-hex16", blocked("30F2EA56E759FB1D"))

    def test_compact_id_is_upper_case_by_design(self):
        # Govee publishes compact ids in upper case. Lower-case 16-hex strings are
        # cargo test-binary hashes in captured output and must stay allowed.
        self.assertEqual(blocked("target/debug/deps/govee-9d5efbfc1c54196e"), [])
        self.assertEqual(blocked("topic gv2mqtt/30f2ea56e759fb1d/command"), [])

    def test_path_exemptions_are_exact(self):
        self.assertEqual(blocked("Codex web agents read this", path="CLAUDE.md"), [])
        self.assertIn("process-codex", blocked("Codex web agents read this", path="docs/x.md"))
        self.assertEqual(blocked(".context/", path=".gitignore"), [])
        self.assertIn("process-context-dir", blocked(".context/", path="docs/x.md"))

    def test_warnings_do_not_block(self):
        text = "a drive-by fix on 10.0.0.1 by Florian"
        self.assertEqual(blocked(text), [])
        self.assertEqual(warned(text), ["net-10-slash-8", "process-drive-by", "process-maintainer-name"])

    def test_every_rule_regex_compiles(self):
        for rule in guard.RULES:
            with self.subTest(rule=rule.id):
                rule.regex()

    def test_every_fixture_value_is_exempt(self):
        for value in sorted(guard.UPSTREAM_FIXTURE_DEVICE_IDS):
            with self.subTest(value=value):
                self.assertEqual(blocked(f"device {value}"), [])
        for value in sorted(guard.FIXTURE_MACS):
            with self.subTest(value=value):
                self.assertEqual(blocked(f"mac {value}"), [])
        for value in sorted(guard.FIXTURE_LAN_IPS):
            with self.subTest(value=value):
                self.assertEqual(blocked(f"ip {value}"), [])

    def test_commit_rules_file_may_name_the_tooling(self):
        text = "codex gstack subagent then /ship and /review"
        self.assertEqual(blocked(text, path=".config/commit-rules.json"), [])
        self.assertEqual(sorted(blocked(text)), [
            "process-codex", "process-gstack", "process-slash-commands", "process-subagent"])

    def test_remaining_path_exemptions(self):
        self.assertEqual(blocked("Codex", path="AUTHOR-NOTES.md"), [])
        self.assertEqual(blocked("scratch in .context/", path="CONTRIBUTING.md"), [])
        self.assertEqual(blocked(".claude/ and .gstack/", path=".gitignore"), [])
        self.assertIn("path-agent-dotdirs", blocked(".claude/ and .gstack/", path="README.md"))

    def test_every_warn_rule_has_a_true_positive(self):
        cases = {
            "process-policy-actor": "per policy no agent opens a PR",
            "process-drive-by": "a drive-by fix",
            "process-specialist-confidence": "the specialist reviewers agreed, confidence 7/10",
            "process-conductor": "opened in conductor",
            "process-skill-word": "the skill did it",
            "process-maintainer-name": "Florian decided",
            "net-10-slash-8": "host 10.1.2.3",
            "net-link-local": "fell back to 169.254.12.9",
            "net-generic-ha-hosts": "http://homeassistant.local:8123",
            "device-mac-dashed": "mac 1D-6D-0C-AA-11-22",
        }
        warn_ids = {rule.id for rule in guard.RULES if not rule.block}
        self.assertEqual(set(cases), warn_ids)
        for rule_id, text in cases.items():
            with self.subTest(rule=rule_id):
                self.assertIn(rule_id, warned(text))
                self.assertEqual(blocked(text), [])


class PrBodyPatterns(unittest.TestCase):
    """scripts/lint-patterns.sh is consumed by the shared PR-body lint as plain grep -E
    patterns with no exemptions, so it must hit the leak classes and spare the
    ordinary contents of a pull request body."""

    BAD = [
        "found by the outside voice",
        "an adversarial review found it",
        "Pre-landing review",
        "run /ship to land it",
        "Codex flagged this",
        "wired through gstack",
        "a coverage subagent",
        "status: DONE_WITH_CONCERNS",
        "scope discipline beats a fix",
        "raised as squad P1",
        "and Florian sends it",
        "drafted to .context/ first",
        "see ~/.claude/hooks",
        "Compiling govee (/Users/someone/repo)",
        "LAN API: ip=192.168.178.40",
        "bound to 172.16.4.2",
        "reach it at 100.98.1.7",
        "ssh ha-green",
        "device 30:F2:EA:56:E7:59:FB:1D",
        "mac 30:F2:EA:56:E7:59",
        "topic gv2mqtt/30F2EA56E759FB1D/command",
        "mail someone@somewhere.org",
    ]
    GOOD = [
        "thanks @peas for #45",
        "pre-shipment inspection",
        "the /shipping route and /reviews page",
        "test binary govee-9d5efbfc1c54196e",
        "digest sha256:52fb90d9a3802c73d2e5fcd1e3fc7ca7643384ce4741a89d85928419ea185740",
        "cargo test --all -- --show-output",
        "ghcr.io/florianhorner/govee2mqtt-amd64:2026.09.16-ca472450",
        "H7124 purifier, H60B0 lamp, 362 tests passed",
    ]

    @classmethod
    def setUpClass(cls):
        script = HERE / "lint-patterns.sh"
        out = subprocess.run(
            ["bash", "-c", f'source "{script}"; printf "%s\\0" "${{LINT_PATTERNS[@]}}"'],
            check=True, capture_output=True, text=True).stdout
        cls.patterns = [p for p in out.split("\0") if p]

    def hits(self, line: str) -> list[str]:
        found = []
        for pattern in self.patterns:
            result = subprocess.run(["grep", "-qE", "--", pattern], input=line + "\n",
                                    capture_output=True, text=True)
            self.assertLessEqual(result.returncode, 1, f"pattern failed to compile: {pattern}")
            if result.returncode == 0:
                found.append(pattern)
        return found

    def test_patterns_loaded(self):
        self.assertGreaterEqual(len(self.patterns), 20)

    def test_known_bad_lines_hit(self):
        for line in self.BAD:
            with self.subTest(line=line):
                self.assertTrue(self.hits(line), line)

    def test_known_good_lines_pass(self):
        for line in self.GOOD:
            with self.subTest(line=line):
                self.assertEqual(self.hits(line), [], line)


class RepoModes(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self.tmp.name)
        hooks = self.repo / ".nohooks"
        hooks.mkdir()
        self.env = {
            **os.environ,
            "GIT_CONFIG_GLOBAL": os.devnull,
            "GIT_AUTHOR_NAME": "t",
            "GIT_AUTHOR_EMAIL": "t@example.com",
            "GIT_COMMITTER_NAME": "t",
            "GIT_COMMITTER_EMAIL": "t@example.com",
        }
        self.git("init", "-q")
        self.git("config", "core.hooksPath", str(hooks))
        (self.repo / "README.md").write_text("# clean\n")
        self.git("add", "README.md")
        self.git("commit", "-q", "-m", "init")

    def tearDown(self):
        self.tmp.cleanup()

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def run_guard(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run([sys.executable, str(GUARD), *args], cwd=self.repo, env=self.env,
                              capture_output=True, text=True)

    def test_clean_repository_passes(self):
        result = self.run_guard("--all")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("passed", result.stdout)

    def test_tracked_backlog_fails_even_when_empty(self):
        (self.repo / "todos.md").write_text("")
        self.git("add", "todos.md")
        self.git("commit", "-q", "-m", "backlog")
        result = self.run_guard("--all")
        self.assertEqual(result.returncode, 1)
        self.assertIn("backlog is private", result.stdout)

    def test_leak_in_tracked_file_fails_with_location(self):
        (self.repo / "notes.md").write_text("line one\nreach it at 192.168.178.9\n")
        self.git("add", "notes.md")
        self.git("commit", "-q", "-m", "notes")
        result = self.run_guard("--all")
        self.assertEqual(result.returncode, 1)
        self.assertIn("notes.md:2: [net-192-168]", result.stdout)

    def test_staged_mode_reads_the_index_not_the_worktree(self):
        (self.repo / "notes.md").write_text("clean\n")
        self.git("add", "notes.md")
        (self.repo / "notes.md").write_text("Codex says hi\n")  # unstaged leak
        self.assertEqual(self.run_guard("--staged").returncode, 0)
        self.git("add", "notes.md")
        self.assertEqual(self.run_guard("--staged").returncode, 1)

    def test_binary_extensions_are_skipped_but_a_nul_does_not_hide_text(self):
        (self.repo / "logo.png").write_bytes(b"\x89PNG\0\0Codex 192.168.178.1\0")
        (self.repo / "notes.log").write_bytes(b"\0hidden LAN API: ip=192.168.178.9\n")
        self.git("add", "logo.png", "notes.log")
        self.git("commit", "-q", "-m", "blob")
        result = self.run_guard("--all")
        self.assertEqual(result.returncode, 1)
        self.assertIn("notes.log:1: [net-192-168]", result.stdout)
        self.assertNotIn("logo.png", result.stdout)

    def test_tracked_symlink_target_is_scanned(self):
        (self.repo / "link.md").symlink_to("/home/alice/private/notes.md")
        self.git("add", "link.md")
        self.git("commit", "-q", "-m", "link")
        result = self.run_guard("--all")
        self.assertEqual(result.returncode, 1)
        self.assertIn("link.md:1: [path-home-dir]", result.stdout)

    def test_no_mode_is_a_usage_error(self):
        self.assertEqual(self.run_guard().returncode, 2)

    def test_explicit_paths_are_scanned(self):
        (self.repo / "leaky.md").write_text("Codex says hi\n")
        (self.repo / "clean.md").write_text("all good\n")
        self.assertEqual(self.run_guard("clean.md").returncode, 0)
        result = self.run_guard("clean.md", "leaky.md")
        self.assertEqual(result.returncode, 1)
        self.assertIn("leaky.md:1: [process-codex]", result.stdout)

    def test_missing_explicit_path_is_an_error_not_a_pass(self):
        result = self.run_guard("does-not-exist.md")
        self.assertEqual(result.returncode, 2)
        self.assertIn("no such file", result.stderr)

    def test_outside_a_git_repository_exits_2(self):
        with tempfile.TemporaryDirectory() as plain:
            result = subprocess.run([sys.executable, str(GUARD), "--all"], cwd=plain,
                                    env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)
        self.assertTrue(result.stderr.startswith("ERROR:"), result.stderr)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
REAL_GIT = shutil.which("git")
REAL_GIT_CLIFF = shutil.which("git-cliff")


class ReleaseFixture:
    def __init__(self) -> None:
        if REAL_GIT is None:
            raise RuntimeError("git is required for the release-script tests")

        self.tempdir = tempfile.TemporaryDirectory(prefix="govee-release-test-")
        self.root = Path(self.tempdir.name)
        self.work = self.root / "work"
        self.remote = self.root / "origin.git"
        self.bin_dir = self.root / "bin"
        self.hooks_dir = self.root / "empty-hooks"
        self.command_log = self.root / "commands.log"

        self.work.mkdir()
        self.bin_dir.mkdir()
        self.hooks_dir.mkdir()
        self._git("init", "--bare", str(self.remote), cwd=self.root)
        self._git("init", "-b", "main", cwd=self.work)
        self._git("config", "core.hooksPath", str(self.hooks_dir), cwd=self.work)
        self._git("config", "user.name", "Release Test", cwd=self.work)
        self._git("config", "user.email", "release-test@example.com", cwd=self.work)

        (self.work / "scripts").mkdir()
        (self.work / "addon").mkdir()
        for relative in (
            "scripts/apply-tag.sh",
            "scripts/prepare-release.sh",
            "scripts/tag-release.sh",
            "scripts/validate-release-publication.sh",
            "scripts/cliff.toml",
            "scripts/test_prepare_release.py",
            "scripts/test_live_2fa.py",
        ):
            source = REPO_ROOT / relative
            target = self.work / relative
            if source.exists():
                shutil.copy2(source, target)

        (self.work / "addon" / "config.yaml").write_text(
            'name: Test Add-on\nversion: "2026.01.01-deadbeef"\nslug: test\n',
            encoding="utf-8",
        )
        (self.work / "addon" / "CHANGELOG.md").write_text(
            "# Changelog\n\n## [2026.01.01-deadbeef]\n",
            encoding="utf-8",
        )
        (self.work / "payload.txt").write_text("baseline\n", encoding="utf-8")
        self._git("add", ".", cwd=self.work)
        self._git("commit", "-m", "test: establish prior release", cwd=self.work)
        self.prior_sha = self.git("rev-parse", "HEAD")
        self.previous_tag = f"2026.01.01-{self.prior_sha[:8]}"
        self._git("tag", self.previous_tag, cwd=self.work)

        self._git("remote", "add", "origin", str(self.remote), cwd=self.work)
        self._git("push", "origin", "main", cwd=self.work)
        self._git("push", "origin", f"refs/tags/{self.previous_tag}", cwd=self.work)

        (self.work / "payload.txt").write_text("release candidate\n", encoding="utf-8")
        self._git("add", "payload.txt", cwd=self.work)
        self._git("commit", "-m", "feat(test): add release candidate", cwd=self.work)
        self.candidate = self.git("rev-parse", "HEAD")
        self.tag = self.git(
            "-c",
            "core.abbrev=8",
            "show",
            "-s",
            "--format=%cd-%h",
            "--date=format:%Y.%m.%d",
            self.candidate,
        )
        self._git("push", "origin", "main", cwd=self.work)
        self._git("switch", "-c", "florianhorner/chore/release-test", cwd=self.work)
        self._git("branch", "-f", "main", self.prior_sha, cwd=self.work)

        self._install_fakes()

    def cleanup(self) -> None:
        self.tempdir.cleanup()

    def _git(self, *args: str, cwd: Path) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [REAL_GIT, *args],
            cwd=cwd,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def git(self, *args: str) -> str:
        return self._git(*args, cwd=self.work).stdout.strip()

    def _write_executable(self, name: str, body: str) -> None:
        path = self.bin_dir / name
        path.write_text(textwrap.dedent(body).lstrip(), encoding="utf-8")
        path.chmod(0o755)

    def _install_fakes(self) -> None:
        self._write_executable(
            "git",
            rf"""
            #!/bin/sh
            printf 'git|%s\n' "$*" >> "$RELEASE_TEST_COMMAND_LOG"
            if [ "${{1-}} ${{2-}} ${{3-}}" = 'remote get-url origin' ]; then
              printf '%s\n' 'https://github.com/florianhorner/govee2mqtt-extended.git'
              exit 0
            fi
            if [ "${{1-}} ${{2-}} ${{3-}} ${{4-}} ${{5-}}" = 'remote get-url --push --all origin' ]; then
              printf '%s\n' "${{RELEASE_TEST_PUSH_URL-https://github.com/florianhorner/govee2mqtt-extended.git}}"
              exit 0
            fi
            if [ "${{1-}}" = push ]; then
              printf 'unexpected git push\n' >&2
              exit 97
            fi
            if [ "${{RELEASE_TEST_FAIL_GIT-}}" = "${{1-}}" ]; then
              printf 'injected git failure: %s\n' "${{1-}}" >&2
              exit 96
            fi
            if [ "${{RELEASE_TEST_FAIL_ROLLBACK_GIT-}}" = "${{1-}}" ]; then
              printf 'injected rollback failure: %s\n' "${{1-}}" >&2
              exit 90
            fi
            exec {shlex_quote(REAL_GIT)} "$@"
            """,
        )
        self._write_executable(
            "gh",
            r"""
            #!/bin/sh
            printf 'gh|%s\n' "$*" >> "$RELEASE_TEST_COMMAND_LOG"
            case "${RELEASE_TEST_FAIL_GH-}:$*" in
              repo:repo\ view*|run-list:run\ list*|run-view:run\ view*|release-list:release\ list*)
                printf 'injected gh failure\n' >&2
                exit 95
                ;;
            esac
            case "$1 $2" in
              'repo view')
                printf '%s\n' 'florianhorner/govee2mqtt-extended|florianhorner|true|wez'
                ;;
              'release list')
                printf '%s\n' "$RELEASE_TEST_PREVIOUS_TAG"
                ;;
              'run list')
                case "${RELEASE_TEST_CI_RUN_MODE-}" in
                  empty) ;;
                  malformed) printf '%s\n' 'not|enough|fields' ;;
                  wrong-head) printf '%s\n' '123|completed|success|deadbeef|https://example.test/actions/123' ;;
                  in-progress) printf '123|in_progress||%s|https://example.test/actions/123\n' "$RELEASE_TEST_CANDIDATE" ;;
                  failed) printf '123|completed|failure|%s|https://example.test/actions/123\n' "$RELEASE_TEST_CANDIDATE" ;;
                  *) printf '123|completed|success|%s|https://example.test/actions/123\n' "$RELEASE_TEST_CANDIDATE" ;;
                esac
                ;;
              'run view')
                case "${RELEASE_TEST_CI_JOBS_MODE-}" in
                  missing)
                    cat <<'EOF'
            build (linux/amd64)|completed|success
            build (linux/arm64)|completed|success
            merge|completed|success
            test-addon (amd64, ubuntu)|completed|success
            EOF
                    ;;
                  duplicate)
                    cat <<'EOF'
            build (linux/amd64)|completed|success
            build (linux/arm64)|completed|success
            merge|completed|success
            merge|completed|success
            test-addon (amd64, ubuntu)|completed|success
            test-addon (aarch64, ubuntu)|completed|success
            EOF
                    ;;
                  skipped)
                    cat <<'EOF'
            build (linux/amd64)|completed|success
            build (linux/arm64)|completed|success
            merge|completed|success
            test-addon (amd64, ubuntu)|completed|success
            test-addon (aarch64, ubuntu)|completed|skipped
            EOF
                    ;;
                  malformed)
                    printf '%s\n' 'not-a-job-row'
                    ;;
                  *)
                    cat <<'EOF'
            build (linux/amd64)|completed|success
            build (linux/arm64)|completed|success
            merge|completed|success
            test-addon (amd64, ubuntu)|completed|success
            test-addon (aarch64, ubuntu)|completed|success
            EOF
                    ;;
                esac
                ;;
              *)
                printf 'unexpected gh invocation: %s\n' "$*" >&2
                exit 94
                ;;
            esac
            """,
        )
        self._write_executable(
            "git-cliff",
            r"""
            #!/bin/sh
            printf 'git-cliff|%s\n' "$*" >> "$RELEASE_TEST_COMMAND_LOG"
            if [ "${1-}" = --version ]; then
              printf '%s\n' 'git-cliff 2.13.1'
              exit 0
            fi
            if [ "${RELEASE_TEST_CLIFF_MODE-}" = fail ]; then
              printf 'injected git-cliff failure\n' >&2
              exit 93
            fi
            tag=
            while [ "$#" -gt 0 ]; do
              if [ "$1" = --tag ]; then
                shift
                tag=$1
              fi
              shift
            done
            if [ "${RELEASE_TEST_CLIFF_MODE-}" = empty ]; then
              exit 0
            fi
            cat <<EOF
            # Changelog

            This file is automatically generated from the git commit history.

            ## [$tag] - 2026-09-03 00:00

            ### Features

            - Test release preparation
            EOF
            if [ "${RELEASE_TEST_CLIFF_MODE-}" = merge ]; then
              printf '%s\n' '- Merge pull request #1 from example/noise'
            fi
            cat <<'EOF'

            <!-- generated by git-cliff -->
            EOF
            """,
        )
        self._write_executable(
            "cargo",
            r"""
            #!/bin/sh
            printf 'cargo|%s\n' "$*" >> "$RELEASE_TEST_COMMAND_LOG"
            case "$*" in
              *"${RELEASE_TEST_FAIL_CARGO-__never__}"*)
                if [ -n "${RELEASE_TEST_FAIL_CARGO-}" ]; then
                  printf 'injected cargo failure: %s\n' "$*" >&2
                  exit 92
                fi
                ;;
            esac
            """,
        )
        self._write_executable(
            "python3",
            r"""
            #!/bin/sh
            printf 'python3|%s\n' "$*" >> "$RELEASE_TEST_COMMAND_LOG"
            if [ "${RELEASE_TEST_FAIL_PYTHON-}" = 1 ]; then
              printf 'injected python failure\n' >&2
              exit 91
            fi
            """,
        )

    def environment(self, **overrides: str) -> dict[str, str]:
        environment = os.environ.copy()
        environment.pop("TAG_NAME", None)
        environment.update(
            {
                "PATH": f"{self.bin_dir}{os.pathsep}{environment['PATH']}",
                "RELEASE_TEST_CANDIDATE": self.candidate,
                "RELEASE_TEST_COMMAND_LOG": str(self.command_log),
                "RELEASE_TEST_PREVIOUS_TAG": self.previous_tag,
            }
        )
        environment.update(overrides)
        return environment

    def run(self, *args: str, **environment: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [str(self.work / "scripts" / "prepare-release.sh"), *args],
            cwd=self.work,
            env=self.environment(**environment),
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def validate_publication(
        self, tag: str | None = None, **environment: str
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                str(self.work / "scripts" / "validate-release-publication.sh"),
                tag or self.tag,
            ],
            cwd=self.work,
            env=self.environment(**environment),
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def commands(self) -> list[str]:
        if not self.command_log.exists():
            return []
        return self.command_log.read_text(encoding="utf-8").splitlines()


def shlex_quote(value: str) -> str:
    return "'" + value.replace("'", "'\\''") + "'"


class PrepareReleaseTests(unittest.TestCase):
    def fixture(self) -> ReleaseFixture:
        fixture = ReleaseFixture()
        self.addCleanup(fixture.cleanup)
        return fixture

    def assert_candidate_untouched(self, fixture: ReleaseFixture) -> None:
        self.assertEqual(fixture.candidate, fixture.git("rev-parse", "HEAD"))
        self.assertEqual(
            "", fixture.git("status", "--porcelain=v1", "--untracked-files=normal")
        )
        self.assertNotIn(fixture.tag, fixture.git("tag", "--list").splitlines())

    def assert_no_external_writes(self, fixture: ReleaseFixture) -> None:
        commands = fixture.commands()
        self.assertFalse(any(command.startswith("git|push ") for command in commands))
        self.assertFalse(
            any(
                command.startswith(prefix)
                for command in commands
                for prefix in (
                    "gh|release create",
                    "gh|release edit",
                    "gh|pr create",
                    "gh|pr merge",
                )
            )
        )

    def test_check_is_read_only_on_a_conductor_branch_with_stale_local_main(
        self,
    ) -> None:
        fixture = self.fixture()

        result = fixture.run("--check", "--expected-head", fixture.candidate)

        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("CHECK PASSED", result.stdout)
        self.assertIn(fixture.tag, result.stdout)
        self.assertEqual(fixture.prior_sha, fixture.git("rev-parse", "main"))
        self.assert_candidate_untouched(fixture)
        self.assert_no_external_writes(fixture)
        cliff_call = next(
            command
            for command in fixture.commands()
            if command.startswith("git-cliff|--offline")
        )
        self.assertIn(f"{fixture.previous_tag}..{fixture.candidate}", cliff_call)

    def test_prepare_creates_only_the_metadata_commit_and_no_tag(self) -> None:
        fixture = self.fixture()

        result = fixture.run("--prepare", "--expected-head", fixture.candidate)

        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("PREPARE PASSED", result.stdout)
        release_commit = fixture.git("rev-parse", "HEAD")
        self.assertNotEqual(fixture.candidate, release_commit)
        self.assertEqual(fixture.candidate, fixture.git("rev-parse", "HEAD^"))
        self.assertEqual(
            ["addon/CHANGELOG.md", "addon/config.yaml"],
            fixture.git(
                "diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"
            ).splitlines(),
        )
        self.assertIn(
            f'version: "{fixture.tag}"',
            (fixture.work / "addon" / "config.yaml").read_text(encoding="utf-8"),
        )
        self.assertIn(
            f"Release-Candidate: {fixture.candidate}",
            fixture.git("show", "-s", "--format=%B", "HEAD"),
        )
        self.assertNotIn(fixture.tag, fixture.git("tag", "--list").splitlines())
        self.assertEqual(
            "", fixture.git("status", "--porcelain=v1", "--untracked-files=normal")
        )
        remote_main = self._remote_ref(fixture, "refs/heads/main")
        self.assertEqual(fixture.candidate, remote_main)
        self.assert_no_external_writes(fixture)

    def test_prepare_rejects_main_and_detached_head(self) -> None:
        on_main = self.fixture()
        on_main._git("branch", "-f", "main", on_main.candidate, cwd=on_main.work)
        on_main._git("switch", "main", cwd=on_main.work)

        main_result = on_main.run("--prepare", "--expected-head", on_main.candidate)

        self.assertNotEqual(0, main_result.returncode)
        self.assertIn("must not create", main_result.stderr)
        self.assert_candidate_untouched(on_main)

        detached = self.fixture()
        detached._git("switch", "--detach", detached.candidate, cwd=detached.work)

        detached_result = detached.run(
            "--prepare", "--expected-head", detached.candidate
        )

        self.assertNotEqual(0, detached_result.returncode)
        self.assertIn("requires an attached", detached_result.stderr)
        self.assert_candidate_untouched(detached)

    def test_owned_fetch_url_does_not_hide_an_unsafe_push_url(self) -> None:
        fixture = self.fixture()

        result = fixture.run(
            "--check",
            "--expected-head",
            fixture.candidate,
            RELEASE_TEST_PUSH_URL="git@github.com:wez/govee2mqtt.git",
        )

        self.assertNotEqual(0, result.returncode)
        self.assertIn("push URL does not point at the owned fork", result.stderr)
        self.assert_candidate_untouched(fixture)
        self.assert_no_external_writes(fixture)

    def test_dirty_states_fail_before_gates_or_mutation(self) -> None:
        for dirty_kind in ("unstaged", "staged", "untracked"):
            with self.subTest(dirty_kind=dirty_kind):
                fixture = self.fixture()
                if dirty_kind == "unstaged":
                    (fixture.work / "payload.txt").write_text(
                        "dirty\n", encoding="utf-8"
                    )
                elif dirty_kind == "staged":
                    (fixture.work / "payload.txt").write_text(
                        "staged\n", encoding="utf-8"
                    )
                    fixture._git("add", "payload.txt", cwd=fixture.work)
                else:
                    (fixture.work / "untracked.txt").write_text(
                        "untracked\n", encoding="utf-8"
                    )

                before = fixture.git("rev-parse", "HEAD")
                result = fixture.run("--prepare", "--expected-head", fixture.candidate)

                self.assertNotEqual(0, result.returncode)
                self.assertIn("completely clean", result.stderr)
                self.assertEqual(before, fixture.git("rev-parse", "HEAD"))
                self.assertFalse(
                    any(command.startswith("cargo|") for command in fixture.commands())
                )
                self.assert_no_external_writes(fixture)

    def test_stale_candidate_and_tag_collisions_fail_closed(self) -> None:
        stale = self.fixture()
        stale._git(
            "--git-dir",
            str(stale.remote),
            "update-ref",
            "refs/heads/main",
            stale.prior_sha,
            cwd=stale.root,
        )
        stale_result = stale.run("--check", "--expected-head", stale.candidate)
        self.assertNotEqual(0, stale_result.returncode)
        self.assertIn("is not the current origin/main", stale_result.stderr)
        self.assert_candidate_untouched(stale)

        local_collision = self.fixture()
        local_collision._git("tag", local_collision.tag, cwd=local_collision.work)
        local_result = local_collision.run(
            "--check", "--expected-head", local_collision.candidate
        )
        self.assertNotEqual(0, local_result.returncode)
        self.assertIn("already exists locally", local_result.stderr)
        self.assertEqual(
            local_collision.candidate, local_collision.git("rev-parse", "HEAD")
        )

        remote_collision = self.fixture()
        remote_collision._git("tag", remote_collision.tag, cwd=remote_collision.work)
        remote_collision._git(
            "push",
            "origin",
            f"refs/tags/{remote_collision.tag}",
            cwd=remote_collision.work,
        )
        remote_collision._git(
            "tag", "-d", remote_collision.tag, cwd=remote_collision.work
        )
        remote_result = remote_collision.run(
            "--check", "--expected-head", remote_collision.candidate
        )
        self.assertNotEqual(0, remote_result.returncode)
        self.assertIn("already exists remotely", remote_result.stderr)
        self.assert_candidate_untouched(remote_collision)

    def test_remote_and_github_failures_are_not_treated_as_absence(self) -> None:
        remote_failure = self.fixture()
        remote_failure._git(
            "remote",
            "set-url",
            "origin",
            str(remote_failure.root / "missing.git"),
            cwd=remote_failure.work,
        )
        result = remote_failure.run(
            "--check", "--expected-head", remote_failure.candidate
        )
        self.assertNotEqual(0, result.returncode)
        self.assertIn("cannot read origin/main", result.stderr)
        self.assert_candidate_untouched(remote_failure)

        gh_failure = self.fixture()
        result = gh_failure.run(
            "--check",
            "--expected-head",
            gh_failure.candidate,
            RELEASE_TEST_FAIL_GH="run-list",
        )
        self.assertNotEqual(0, result.returncode)
        self.assertIn("cannot read Container Build", result.stderr)
        self.assert_candidate_untouched(gh_failure)

    def test_malformed_or_unsuccessful_ci_evidence_fails_closed(self) -> None:
        run_cases = (
            ("empty", "no Container Build run exists"),
            ("malformed", "malformed Container Build data"),
            ("wrong-head", "not bound to the candidate SHA"),
            ("in-progress", "has not completed"),
            ("failed", "did not succeed"),
        )
        for mode, expected_error in run_cases:
            with self.subTest(ci_run_mode=mode):
                fixture = self.fixture()
                result = fixture.run(
                    "--check",
                    "--expected-head",
                    fixture.candidate,
                    RELEASE_TEST_CI_RUN_MODE=mode,
                )
                self.assertNotEqual(0, result.returncode)
                self.assertIn(expected_error, result.stderr)
                self.assert_candidate_untouched(fixture)
                self.assert_no_external_writes(fixture)

        for mode in ("missing", "duplicate", "skipped", "malformed"):
            with self.subTest(ci_jobs_mode=mode):
                fixture = self.fixture()
                result = fixture.run(
                    "--check",
                    "--expected-head",
                    fixture.candidate,
                    RELEASE_TEST_CI_JOBS_MODE=mode,
                )
                self.assertNotEqual(0, result.returncode)
                self.assertIn("required Container Build job", result.stderr)
                self.assert_candidate_untouched(fixture)
                self.assert_no_external_writes(fixture)

    def test_gate_and_commit_failures_restore_the_candidate(self) -> None:
        for environment in (
            {"RELEASE_TEST_FAIL_CARGO": "clippy"},
            {"RELEASE_TEST_FAIL_PYTHON": "1"},
            {"RELEASE_TEST_FAIL_GIT": "add"},
            {"RELEASE_TEST_FAIL_GIT": "commit"},
            {"RELEASE_TEST_FAIL_GIT": "diff-tree"},
        ):
            with self.subTest(environment=environment):
                fixture = self.fixture()
                result = fixture.run(
                    "--prepare",
                    "--expected-head",
                    fixture.candidate,
                    **environment,
                )
                self.assertNotEqual(0, result.returncode)
                self.assert_candidate_untouched(fixture)
                self.assert_no_external_writes(fixture)

    def test_rollback_failures_are_reported_and_leave_evidence(self) -> None:
        reset_failure = self.fixture()
        result = reset_failure.run(
            "--prepare",
            "--expected-head",
            reset_failure.candidate,
            RELEASE_TEST_FAIL_GIT="diff-tree",
            RELEASE_TEST_FAIL_ROLLBACK_GIT="reset",
        )
        self.assertNotEqual(0, result.returncode)
        self.assertIn("ROLLBACK FAILED", result.stderr)
        self.assertNotEqual(
            reset_failure.candidate, reset_failure.git("rev-parse", "HEAD")
        )
        self.assert_no_external_writes(reset_failure)

        restore_failure = self.fixture()
        result = restore_failure.run(
            "--prepare",
            "--expected-head",
            restore_failure.candidate,
            RELEASE_TEST_FAIL_GIT="add",
            RELEASE_TEST_FAIL_ROLLBACK_GIT="restore",
        )
        self.assertNotEqual(0, result.returncode)
        self.assertIn("ROLLBACK FAILED", result.stderr)
        self.assertEqual(
            restore_failure.candidate, restore_failure.git("rev-parse", "HEAD")
        )
        self.assertNotEqual(
            "",
            restore_failure.git("status", "--porcelain=v1", "--untracked-files=normal"),
        )
        self.assert_no_external_writes(restore_failure)

    def test_bad_changelog_and_tag_override_fail_without_mutation(self) -> None:
        for cliff_mode in ("fail", "empty", "merge"):
            with self.subTest(cliff_mode=cliff_mode):
                fixture = self.fixture()
                result = fixture.run(
                    "--prepare",
                    "--expected-head",
                    fixture.candidate,
                    RELEASE_TEST_CLIFF_MODE=cliff_mode,
                )
                self.assertNotEqual(0, result.returncode)
                self.assert_candidate_untouched(fixture)

        override = self.fixture()
        result = override.run(
            "--prepare",
            "--expected-head",
            override.candidate,
            TAG_NAME="2026.09.03-deadbeef",
        )
        self.assertNotEqual(0, result.returncode)
        self.assertIn("TAG_NAME must equal", result.stderr)
        self.assert_candidate_untouched(override)

    def test_apply_tag_rejects_duplicate_version_keys(self) -> None:
        fixture = self.fixture()
        config_path = fixture.work / "addon" / "config.yaml"
        config_path.write_text(
            config_path.read_text(encoding="utf-8") + 'version: "duplicate"\n',
            encoding="utf-8",
        )
        fixture._git("add", "addon/config.yaml", cwd=fixture.work)
        fixture._git(
            "commit", "-m", "test: add duplicate version key", cwd=fixture.work
        )
        duplicate_candidate = fixture.git("rev-parse", "HEAD")
        fixture._git("push", "origin", "HEAD:main", cwd=fixture.work)

        result = subprocess.run(
            [str(fixture.work / "scripts" / "apply-tag.sh")],
            cwd=fixture.work,
            env=fixture.environment(RELEASE_TEST_CANDIDATE=duplicate_candidate),
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

        self.assertNotEqual(0, result.returncode)
        self.assertIn("exactly one root version key", result.stderr)
        self.assertEqual(2, config_path.read_text(encoding="utf-8").count("version:"))

    def test_publication_preflight_binds_tag_to_current_remote_main(self) -> None:
        fixture = self.fixture()

        result = fixture.validate_publication()

        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("RELEASE PREFLIGHT PASSED", result.stdout)
        self.assertIn(fixture.tag, result.stdout)
        self.assertIn(fixture.candidate, result.stdout)
        self.assert_no_external_writes(fixture)

    def test_publication_preflight_rejects_wrong_tag_and_stale_main(self) -> None:
        wrong_tag = self.fixture()
        result = wrong_tag.validate_publication("2026.09.14-deadbeef")
        self.assertNotEqual(0, result.returncode)
        self.assertIn("does not match HEAD-derived tag", result.stderr)
        self.assert_no_external_writes(wrong_tag)

        stale_main = self.fixture()
        stale_main._git(
            "--git-dir",
            str(stale_main.remote),
            "update-ref",
            "refs/heads/main",
            stale_main.prior_sha,
            cwd=stale_main.root,
        )
        result = stale_main.validate_publication()
        self.assertNotEqual(0, result.returncode)
        self.assertIn("is not the current origin/main", result.stderr)
        self.assert_no_external_writes(stale_main)

    def test_publication_preflight_precedes_registry_writes(self) -> None:
        workflow = (REPO_ROOT / ".github" / "workflows" / "build.yml").read_text(
            encoding="utf-8"
        )
        preflight_start = workflow.index("  release-preflight:")
        build_start = workflow.index("  build:", preflight_start)
        merge_start = workflow.index("  merge:", build_start)
        preflight = workflow[preflight_start:build_start]
        build = workflow[build_start:merge_start]

        self.assertIn("./scripts/validate-release-publication.sh", preflight)
        self.assertNotIn("github.event_name", preflight)
        self.assertIn("github.ref_type == 'tag'", preflight)
        self.assertIn("needs:\n      - release-preflight", build)
        self.assertIn("TAG_NAME: ${{ github.ref_name }}", workflow)

    def test_nochangelog_skip_precedes_git_cliff_grouping(self) -> None:
        config = (REPO_ROOT / "scripts" / "cliff.toml").read_text(encoding="utf-8")
        skip_position = config.index('{ body = "NOCHANGELOG", skip = true }')
        for grouping_rule in (
            '{ message = "^feat", group =',
            '{ message = "^fix", group =',
            '{ message = "^(chore|ci)", group =',
            '{ body = ".*security", group =',
            '{ message = ".*", group =',
        ):
            self.assertLess(skip_position, config.index(grouping_rule))

    def test_real_git_cliff_skips_the_release_metadata_commit(self) -> None:
        if REAL_GIT_CLIFF is None:
            self.skipTest("git-cliff is not installed")
        version = subprocess.run(
            [REAL_GIT_CLIFF, "--version"],
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        ).stdout.strip()
        if version != "git-cliff 2.13.1":
            self.skipTest(f"git-cliff 2.13.1 is required, found {version}")

        fixture = self.fixture()
        (fixture.work / "payload.txt").write_text(
            "release candidate\nmetadata\n", encoding="utf-8"
        )
        fixture._git("add", "payload.txt", cwd=fixture.work)
        fixture._git(
            "commit",
            "-m",
            "chore(release): prepare 2026.02.01-cafebabe",
            "-m",
            "NOCHANGELOG\nRelease-Candidate: cafebabe",
            cwd=fixture.work,
        )
        release_commit = fixture.git("rev-parse", "HEAD")

        result = subprocess.run(
            [
                REAL_GIT_CLIFF,
                "--offline",
                "--repository",
                str(fixture.work),
                "--config",
                str(REPO_ROOT / "scripts" / "cliff.toml"),
                "--tag",
                "2026.02.01-cafebabe",
                f"{fixture.previous_tag}..{release_commit}",
            ],
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

        self.assertNotIn("Prepare 2026.02.01-cafebabe", result.stdout)
        self.assertNotIn("chore(release): prepare", result.stdout)

    def _remote_ref(self, fixture: ReleaseFixture, ref: str) -> str:
        result = fixture._git("ls-remote", "origin", ref, cwd=fixture.work)
        return result.stdout.split()[0]


if __name__ == "__main__":
    unittest.main()

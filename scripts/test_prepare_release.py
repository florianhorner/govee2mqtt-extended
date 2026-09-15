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
REAL_DOCKER = shutil.which("docker")
GIT_CLIFF_IMAGE = (
    "ghcr.io/orhun/git-cliff/git-cliff:2.13.1@"
    "sha256:d49216b61658fc1b10bab6c5f82dfca03b8e37278618fdc3db235d95cf3c33f5"
)


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
        self.previous_tag_commit = self.git("rev-parse", "HEAD")
        self.previous_tag = f"2026.01.01-{self.previous_tag_commit[:8]}"
        self._git("tag", self.previous_tag, cwd=self.work)

        (self.work / "addon" / "config.yaml").write_text(
            f'name: Test Add-on\nversion: "{self.previous_tag}"\nslug: test\n',
            encoding="utf-8",
        )
        (self.work / "addon" / "CHANGELOG.md").write_text(
            f"# Changelog\n\n## [{self.previous_tag}]\n", encoding="utf-8"
        )
        self._git("add", "addon/config.yaml", "addon/CHANGELOG.md", cwd=self.work)
        self._git(
            "commit",
            "-m",
            f"chore(release): prepare {self.previous_tag}",
            "-m",
            "NOCHANGELOG",
            cwd=self.work,
        )
        self.prior_sha = self.git("rev-parse", "HEAD")

        self._git("remote", "add", "origin", str(self.remote), cwd=self.work)
        self._git("push", "origin", "main", cwd=self.work)
        self._git("push", "origin", f"refs/tags/{self.previous_tag}", cwd=self.work)

        (self.work / "payload.txt").write_text("release candidate\n", encoding="utf-8")
        self._git("add", "payload.txt", cwd=self.work)
        self._git("commit", "-m", "feat(test): add release candidate", cwd=self.work)
        self.candidate = self.git("rev-parse", "HEAD")
        self.tag = self.release_tag(self.candidate)
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

    def release_tag(self, candidate: str) -> str:
        release_date = self.git(
            "show", "-s", "--format=%cd", "--date=format:%Y.%m.%d", candidate
        )
        return f"{release_date}-{candidate[:8]}"

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

    def test_check_is_read_only_with_stale_local_main(self) -> None:
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

    def test_addon_version_selects_changelog_base_over_newer_20_tag(self) -> None:
        fixture = self.fixture()
        aborted_tag = "20-aborted-candidate"
        fixture._git("tag", aborted_tag, fixture.candidate, cwd=fixture.work)
        fixture._git(
            "push", "origin", f"refs/tags/{aborted_tag}", cwd=fixture.work
        )

        result = fixture.run("--check", "--expected-head", fixture.candidate)

        self.assertEqual(0, result.returncode, result.stderr)
        cliff_call = next(
            command
            for command in fixture.commands()
            if command.startswith("git-cliff|--offline")
        )
        self.assertIn(f"{fixture.previous_tag}..{fixture.candidate}", cliff_call)
        self.assertNotIn(f"{aborted_tag}..{fixture.candidate}", cliff_call)
        self.assert_candidate_untouched(fixture)
        self.assert_no_external_writes(fixture)

    def test_addon_version_baseline_must_resolve_and_be_ancestor(self) -> None:
        missing = self.fixture()
        missing._git("tag", "-d", missing.previous_tag, cwd=missing.work)

        result = missing.run("--check", "--expected-head", missing.candidate)

        self.assertNotEqual(0, result.returncode)
        self.assertIn("add-on baseline tag is missing locally", result.stderr)
        self.assert_candidate_untouched(missing)
        self.assert_no_external_writes(missing)

        missing_remote = self.fixture()
        missing_remote._git(
            "--git-dir",
            str(missing_remote.remote),
            "update-ref",
            "-d",
            f"refs/tags/{missing_remote.previous_tag}",
            cwd=missing_remote.root,
        )

        result = missing_remote.run(
            "--check", "--expected-head", missing_remote.candidate
        )

        self.assertNotEqual(0, result.returncode)
        self.assertIn("add-on baseline tag is missing remotely", result.stderr)
        self.assert_candidate_untouched(missing_remote)
        self.assert_no_external_writes(missing_remote)

        nonancestor = self.fixture()
        unrelated = nonancestor.git(
            "commit-tree", "HEAD^{tree}", "-m", "test: unrelated release"
        )
        unrelated_tag = f"2026.02.02-{unrelated[:8]}"
        nonancestor._git("tag", unrelated_tag, unrelated, cwd=nonancestor.work)
        nonancestor._git(
            "push", "origin", f"refs/tags/{unrelated_tag}", cwd=nonancestor.work
        )
        config_path = nonancestor.work / "addon" / "config.yaml"
        config_path.write_text(
            f'name: Test Add-on\nversion: "{unrelated_tag}"\nslug: test\n',
            encoding="utf-8",
        )
        nonancestor._git("add", "addon/config.yaml", cwd=nonancestor.work)
        nonancestor._git(
            "commit", "-m", "test: select unrelated baseline", cwd=nonancestor.work
        )
        nonancestor.candidate = nonancestor.git("rev-parse", "HEAD")
        nonancestor.tag = nonancestor.release_tag(nonancestor.candidate)
        nonancestor._git("push", "origin", "HEAD:main", cwd=nonancestor.work)

        result = nonancestor.run(
            "--check", "--expected-head", nonancestor.candidate
        )

        self.assertNotEqual(0, result.returncode)
        self.assertIn("is not an ancestor of the candidate", result.stderr)
        self.assert_candidate_untouched(nonancestor)
        self.assert_no_external_writes(nonancestor)

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
        self.assertEqual(
            [
                "cargo|build --all",
                "cargo|clippy --all -- -D warnings",
                "cargo|clippy --all --all-features -- -D warnings",
                "cargo|test --all -- --show-output --test-threads=1",
                "cargo|test --all --all-features -- --show-output --test-threads=1",
                "python3|-m unittest scripts/test_live_2fa.py scripts/test_prepare_release.py",
                "cargo|fmt --all -- --check",
            ],
            [
                command
                for command in fixture.commands()
                if command.startswith(("cargo|", "python3|"))
            ],
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

    def test_release_tag_uses_fixed_full_sha_prefix_in_every_script(self) -> None:
        for relative in (
            "scripts/apply-tag.sh",
            "scripts/prepare-release.sh",
            "scripts/validate-release-publication.sh",
        ):
            with self.subTest(script=relative):
                script = (REPO_ROOT / relative).read_text(encoding="utf-8")
                self.assertNotIn("%h", script)
                self.assertIn("candidate_prefix=$(printf '%.8s'", script)
                self.assertIn("[0-9a-f]{8}$", script)

        build_script = (REPO_ROOT / "build.rs").read_text(encoding="utf-8")
        self.assertNotIn("%h", build_script)
        self.assertIn('"--format=%cd%n%H"', build_script)
        self.assertIn("commit.get(..8)", build_script)

    def test_tag_release_delegates_arguments_to_prepare(self) -> None:
        fixture = self.fixture()

        result = subprocess.run(
            [
                str(fixture.work / "scripts" / "tag-release.sh"),
                "--check",
                "--expected-head",
                fixture.candidate,
            ],
            cwd=fixture.work,
            env=fixture.environment(),
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("CHECK PASSED", result.stdout)
        self.assert_candidate_untouched(fixture)
        self.assert_no_external_writes(fixture)

    def test_publication_preflight_binds_tag_to_current_remote_main(self) -> None:
        for annotated in (False, True):
            with self.subTest(annotated=annotated):
                fixture = self.fixture()
                tag_args = ["tag"]
                if annotated:
                    tag_args.extend(("-a", "-m", "release test"))
                tag_args.extend((fixture.tag, fixture.candidate))
                fixture._git(*tag_args, cwd=fixture.work)
                fixture._git(
                    "push", "origin", f"refs/tags/{fixture.tag}", cwd=fixture.work
                )

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
        stale_main._git("tag", stale_main.tag, stale_main.candidate, cwd=stale_main.work)
        stale_main._git(
            "push", "origin", f"refs/tags/{stale_main.tag}", cwd=stale_main.work
        )
        result = stale_main.validate_publication()
        self.assertNotEqual(0, result.returncode)
        self.assertIn("is not the current origin/main", result.stderr)
        self.assert_no_external_writes(stale_main)

    def test_publication_preflight_rejects_absent_or_moved_remote_tag(self) -> None:
        absent = self.fixture()
        result = absent.validate_publication()
        self.assertNotEqual(0, result.returncode)
        self.assertIn("cannot resolve release tag on origin", result.stderr)
        self.assert_no_external_writes(absent)

        for annotated in (False, True):
            with self.subTest(moved_annotated=annotated):
                moved = self.fixture()
                tag_args = ["tag"]
                if annotated:
                    tag_args.extend(("-a", "-m", "moved release test"))
                tag_args.extend((moved.tag, moved.prior_sha))
                moved._git(*tag_args, cwd=moved.work)
                moved._git(
                    "push", "origin", f"refs/tags/{moved.tag}", cwd=moved.work
                )
                result = moved.validate_publication()
                self.assertNotEqual(0, result.returncode)
                self.assertIn("release tag resolves to", result.stderr)
                self.assert_no_external_writes(moved)

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

        pinned_checkout = (
            "actions/checkout@df4cb1c069e1874edd31b4311f1884172cec0e10"
        )
        self.assertEqual(4, workflow.count(pinned_checkout))
        self.assertEqual(
            4,
            sum(
                line.strip() == "persist-credentials: false"
                for line in workflow.splitlines()
            ),
        )
        self.assertNotIn("uses: actions/checkout@v6", workflow)
        self.assertFalse(
            any(line.lstrip().startswith("ref:") for line in workflow.splitlines())
        )

        build_executable = build.index("      - name: Build executable")
        registry_login = build.index("      - name: Log in to the Container registry")
        image_push = build.index("      - name: Build and push by digest")
        self.assertLess(build_executable, registry_login)
        self.assertLess(registry_login, image_push)

    def test_pr_workflow_syntax_checks_each_release_script(self) -> None:
        workflow = (REPO_ROOT / ".github" / "workflows" / "pr.yml").read_text(
            encoding="utf-8"
        )
        loop_start = workflow.index("        for script in \\\n")
        loop_end = workflow.index("        done", loop_start)
        syntax_loop = workflow[loop_start:loop_end]
        for script in (
            "scripts/apply-tag.sh",
            "scripts/prepare-release.sh",
            "scripts/tag-release.sh",
            "scripts/validate-release-publication.sh",
        ):
            self.assertIn(script, syntax_loop)
        self.assertIn('sh -n "$script"', syntax_loop)
        self.assertIn('dash -n "$script"', syntax_loop)
        self.assertNotIn("sh -n scripts/apply-tag.sh scripts/prepare-release.sh", workflow)
        self.assertNotIn("dash -n scripts/apply-tag.sh scripts/prepare-release.sh", workflow)

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
        prepare_script = (REPO_ROOT / "scripts" / "prepare-release.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn(f"git_cliff_image='{GIT_CLIFF_IMAGE}'", prepare_script)

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

        cliff_args = [
            "--offline",
            "--repository",
            str(fixture.work),
            "--config",
            str(REPO_ROOT / "scripts" / "cliff.toml"),
            "--tag",
            "2026.02.01-cafebabe",
            f"{fixture.previous_tag}..{release_commit}",
        ]
        cliff_command: list[str]
        if REAL_GIT_CLIFF is not None:
            version = subprocess.run(
                [REAL_GIT_CLIFF, "--version"],
                check=True,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            ).stdout.strip()
            if version == "git-cliff 2.13.1":
                cliff_command = [REAL_GIT_CLIFF, *cliff_args]
            else:
                cliff_command = self._docker_git_cliff_command(fixture, release_commit)
        else:
            cliff_command = self._docker_git_cliff_command(fixture, release_commit)

        result = subprocess.run(
            cliff_command,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

        self.assertNotIn("Prepare 2026.02.01-cafebabe", result.stdout)
        self.assertNotIn("chore(release): prepare", result.stdout)

    def _docker_git_cliff_command(
        self, fixture: ReleaseFixture, release_commit: str
    ) -> list[str]:
        if REAL_DOCKER is None:
            self.skipTest("git-cliff 2.13.1 or Docker is required")
        docker_info = subprocess.run(
            [REAL_DOCKER, "info"],
            check=False,
            text=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        if docker_info.returncode != 0:
            self.skipTest("git-cliff 2.13.1 or a running Docker daemon is required")
        git_common_dir = fixture.git(
            "rev-parse", "--path-format=absolute", "--git-common-dir"
        )
        return [
            REAL_DOCKER,
            "run",
            "--rm",
            "--network",
            "none",
            "--mount",
            f"type=bind,src={git_common_dir},dst=/repo.git,readonly",
            "--mount",
            f"type=bind,src={REPO_ROOT / 'scripts' / 'cliff.toml'},dst=/cliff.toml,readonly",
            GIT_CLIFF_IMAGE,
            "--offline",
            "--repository",
            "/repo.git",
            "--config",
            "/cliff.toml",
            "--tag",
            "2026.02.01-cafebabe",
            f"{fixture.previous_tag}..{release_commit}",
        ]

    def _remote_ref(self, fixture: ReleaseFixture, ref: str) -> str:
        result = fixture._git("ls-remote", "origin", ref, cwd=fixture.work)
        return result.stdout.split()[0]


if __name__ == "__main__":
    unittest.main()

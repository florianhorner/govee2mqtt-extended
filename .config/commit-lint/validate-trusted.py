#!/usr/bin/env python3
"""Validate PR text with policy from the trusted pull_request_target checkout.

Only Git objects enter the temporary bare repository. PR files are never checked
out or executed. The token is used for fixed GitHub read endpoints and transport
only; the existing policy runner receives neither credentials nor inherited Git
or shell configuration. No status/check write permission is needed.
"""

from __future__ import annotations

import base64
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request

MAX_JSON = 4 * 1024 * 1024
SHA = re.compile(r"[0-9a-f]{40}\Z")
REPOSITORY = re.compile(r"[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9_.-]{1,100}\Z")
LOGIN = re.compile(r"[A-Za-z0-9][A-Za-z0-9-]{0,99}(?:\[bot\])?\Z")
POLICY_FILES = ("validate-trusted.py", "validate-pr.sh", "commit-msg")


class Unusable(Exception):
    """A fixed, safe diagnostic; never include untrusted text in its message."""


def sha(value: object) -> str:
    if not isinstance(value, str) or not SHA.fullmatch(value) or value == "0" * 40:
        raise Unusable("Invalid revision metadata.")
    return value


def snapshot(data: object, repository: str, number: int) -> tuple[str, str, str, str]:
    try:
        if (type(data["number"]) is not int or data["number"] != number
                or data["state"] != "open"
                or data["base"]["repo"]["full_name"] != repository):
            raise Unusable("Pull request identity or state changed.")
        title, author = data["title"], data["user"]["login"]
        if (not isinstance(title, str) or not 1 <= len(title) <= 1024
                or "\0" in title or any(0xD800 <= ord(char) <= 0xDFFF for char in title)
                or not isinstance(author, str) or not LOGIN.fullmatch(author)):
            raise Unusable("Invalid pull request text metadata.")
        return sha(data["head"]["sha"]), sha(data["base"]["sha"]), title, author
    except (KeyError, TypeError):
        raise Unusable("Incomplete pull request metadata.") from None


def isolated_environment(directory: Path) -> dict[str, str]:
    # An allowlist also removes BASH_ENV, Python import paths, Git alternates,
    # askpass/proxy settings, command overrides, and every inherited credential.
    return {
        "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8",
        "HOME": str(directory), "XDG_CONFIG_HOME": str(directory),
        "TMPDIR": str(directory), "RUNNER_TEMP": str(directory),
        "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_SYSTEM": os.devnull,
        "GIT_CONFIG_GLOBAL": os.devnull, "GIT_TERMINAL_PROMPT": "0",
        "GIT_ASKPASS": "/bin/false", "GIT_NO_REPLACE_OBJECTS": "1",
        "GIT_ATTR_NOSYSTEM": "1",
    }


def process(arguments: list[str], cwd: Path, env: dict[str, str],
            timeout: int = 60, capture: bool = True) -> tuple[int, bytes]:
    try:
        with subprocess.Popen(arguments, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                              stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL, start_new_session=True) as child:
            try:
                output, _ = child.communicate(timeout=timeout)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.communicate()
                raise Unusable("Validation operation timed out.") from None
            return child.returncode, output or b""
    except OSError:
        raise Unusable("Validation operation could not run.") from None


def git(arguments: list[str], cwd: Path, env: dict[str, str],
        timeout: int = 60, capture: bool = True) -> bytes:
    code, output = process([
        "git", "-c", f"core.hooksPath={os.devnull}",
        "-c", "protocol.allow=never", "-c", "protocol.https.allow=always",
        "-c", "http.followRedirects=false", "-c", "credential.helper=",
        "-c", "gc.auto=0", *arguments,
    ], cwd, env, timeout, capture)
    if code:
        raise Unusable("Git metadata or commit range is unavailable.")
    return output


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise Unusable("GitHub API redirects are not allowed.")


def read_pull(repository: str, number: int, token: str) -> object:
    request = urllib.request.Request(
        f"https://api.github.com/repos/{repository}/pulls/{number}",
        headers={"Authorization": f"Bearer {token}",
                 "Accept": "application/vnd.github+json",
                 "X-GitHub-Api-Version": "2022-11-28"})
    try:
        # No ambient proxy configuration or redirects can move credentials to
        # an endpoint selected by PR content or the execution environment.
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
        with opener.open(request, timeout=30) as response:
            payload = response.read(MAX_JSON + 1)
        if len(payload) > MAX_JSON:
            raise Unusable("GitHub API response exceeded the size limit.")
        return json.loads(payload)
    except (OSError, ValueError, RecursionError, urllib.error.URLError):
        raise Unusable("GitHub pull request metadata is unavailable.") from None


def fetch_objects(repository: str, number: int, base: str, bare: Path,
                  env: dict[str, str], token: str) -> None:
    # Authentication is process-local configuration, never an argv value or a
    # stored repository setting. No remote from the PR is followed or stored.
    authorization = base64.b64encode(f"x-access-token:{token}".encode()).decode()
    transport_env = {**env, "GIT_CONFIG_COUNT": "1",
                     "GIT_CONFIG_KEY_0": "http.https://github.com/.extraheader",
                     "GIT_CONFIG_VALUE_0": f"AUTHORIZATION: basic {authorization}"}
    git(["fetch", "--no-tags", "--no-recurse-submodules", "--no-write-fetch-head",
         f"https://github.com/{repository}.git",
         f"+refs/pull/{number}/head:refs/heads/pull", base],
        bare, transport_env, timeout=180, capture=False)


def validate(environ: dict[str, str], policy_dir: Path) -> int:
    if environ.get("GITHUB_EVENT_NAME") != "pull_request_target":
        raise Unusable("The trusted pull request event is required.")
    repository = environ.get("GITHUB_REPOSITORY", "")
    if not REPOSITORY.fullmatch(repository) or repository.split("/")[-1] in (".", ".."):
        raise Unusable("Invalid repository identity.")
    trusted_sha = sha(environ.get("GITHUB_SHA"))
    token = environ.get("GITHUB_TOKEN", "")
    if not token or len(token) > 4096 or any(ord(char) < 33 or ord(char) > 126 for char in token):
        raise Unusable("A read-only GitHub token is required.")
    try:
        with Path(environ["GITHUB_EVENT_PATH"]).open("rb") as event_file:
            payload = event_file.read(MAX_JSON + 1)
        if len(payload) > MAX_JSON:
            raise Unusable("Event metadata exceeded the size limit.")
        event = json.loads(payload)
        number = event["number"]
        if (type(number) is not int or not 0 < number <= 2147483647
                or event["repository"]["full_name"] != repository):
            raise Unusable("Invalid event repository or pull request number.")
        expected = snapshot(event["pull_request"], repository, number)
    except (OSError, ValueError, RecursionError, KeyError, TypeError):
        raise Unusable("Event metadata is unavailable or malformed.") from None
    head, base, title, author = expected
    policy_dir = policy_dir.absolute()
    root = policy_dir.parent.parent
    workspace = Path(environ.get("GITHUB_WORKSPACE", ""))
    if (policy_dir.name != "commit-lint" or policy_dir.parent.name != ".config"
            or policy_dir.is_symlink() or policy_dir.parent.is_symlink()
            or not workspace.is_absolute()
            or root.resolve() != (workspace / "trusted-policy").resolve()):
        raise Unusable("Trusted policy checkout is unavailable.")

    with tempfile.TemporaryDirectory(prefix="trusted-commit-policy-") as temporary:
        scratch = Path(temporary)
        env = isolated_environment(scratch)
        if git(["rev-parse", "HEAD"], root, env).strip() != trusted_sha.encode():
            raise Unusable("Trusted checkout revision does not match the event.")
        if git(["rev-parse", "--show-toplevel"], root, env).strip() != str(root.resolve()).encode():
            raise Unusable("Trusted checkout root does not match the event.")
        for name in POLICY_FILES:
            path = policy_dir / name
            if not path.is_file() or path.is_symlink():
                raise Unusable("Trusted policy file is missing.")
            if path.read_bytes() != git(["show", f"{trusted_sha}:.config/commit-lint/{name}"], root, env):
                raise Unusable("Trusted policy differs from the checked-out revision.")
        if snapshot(read_pull(repository, number, token), repository, number) != expected:
            raise Unusable("Pull request changed before validation; rerun the check.")

        bare = scratch / "objects.git"
        empty_template = scratch / "empty-template"
        empty_template.mkdir()
        git(["init", "--bare", f"--template={empty_template}", str(bare)], scratch, env)
        fetch_objects(repository, number, base, bare, env, token)
        if git(["rev-parse", "refs/heads/pull^{commit}"], bare, env).strip() != head.encode():
            raise Unusable("Fetched pull request revision does not match the event.")
        git(["cat-file", "-e", f"{base}^{{commit}}"], bare, env)
        git(["merge-base", head, base], bare, env)
        git(["symbolic-ref", "HEAD", "refs/heads/pull"], bare, env)
        # The fresh bare repository contains only controller-created settings.
        # No inherited Git configuration overrides or auth reach the runner.
        runner_env = {**env, "PR_BASE_SHA": base, "PR_HEAD_SHA": head,
                      "PR_TITLE": title, "PR_AUTHOR": author}
        code, _ = process(["/bin/bash", str(policy_dir / "validate-pr.sh")],
                          bare, runner_env, timeout=300, capture=False)
        if snapshot(read_pull(repository, number, token), repository, number) != expected:
            raise Unusable("Pull request changed during validation; rerun the check.")
        if code not in (0, 1):
            raise Unusable("Trusted policy could not validate the commit range.")
        return code


def main() -> int:
    try:
        code = validate(dict(os.environ), Path(__file__).absolute().parent)
        print("Commit policy passed." if code == 0 else "::error::Commit policy failed.")
        return code
    except (Unusable, OSError, ValueError) as error:
        # Unexpected filesystem/encoding errors must not disclose paths or text.
        diagnostic = str(error) if isinstance(error, Unusable) else "Trusted validation is unavailable."
        print(f"::error::{diagnostic}")
        return 2


if __name__ == "__main__":
    sys.exit(main())

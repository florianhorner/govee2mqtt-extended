#!/usr/bin/env python3
"""Run the bounded, redacted Govee 2FA live test."""

from __future__ import annotations

import argparse
import contextlib
import dataclasses
import email
import fcntl
import hashlib
import hmac
import imaplib
import json
import os
import platform
import re
import shutil
import ssl
import stat
import subprocess
import sys
import tempfile
import time
import xml.etree.ElementTree as ET
from datetime import datetime, timezone
from email.policy import default as email_policy
from email.utils import parseaddr
from html.parser import HTMLParser
from pathlib import Path
from typing import Dict, List, Optional, Pattern, Sequence


MAX_LOGIN_REQUESTS = 6
MAX_VERIFICATION_REQUESTS = 4
EXPECTED_EMAILS = 2
MAX_HEADER_BYTES = 64 * 1024
MAX_MESSAGE_BYTES = 128 * 1024
MAX_SUBJECT_CHARS = 512
MIN_REDACTED_SECRET_LENGTH = 6
# One probe invocation can make two sequential HTTP requests inside the Rust
# binary, each with its own 30s timeout: the login POST, and -- only on a
# no-code 454 -- the verification-email POST right after it. A network that
# is slow but still within both of those timeouts can legitimately take
# close to 60s end to end. Killing the subprocess sooner than that can cut
# off a request Govee already accepted, reporting login_probe_execution_failed
# for a run that actually sent an email.
PROBE_TIMEOUT_SECONDS = 75
CONFIRMATION = "allow-10-requests-4-email-triggers"
EVENT_FIELDS = {
    "code_configured",
    "email_count",
    "event",
    "message_hash",
    "outcome",
    "probe_number",
    "reason",
    "schema",
    "sequence",
    "status",
    "time_utc",
    "elapsed_ms",
    "window_seconds",
}


class LiveTestError(Exception):
    """A controlled failure safe to include in the evidence bundle."""

    def __init__(self, reason: str):
        super().__init__(reason)
        self.reason = reason


def utc_now() -> str:
    return (
        datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
    )


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run_checked(command: Sequence[str], cwd: Path, timeout: int = 60) -> str:
    try:
        result = subprocess.run(
            list(command),
            cwd=str(cwd),
            check=True,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise LiveTestError("local_command_failed") from error
    return result.stdout.strip()


def ensure_clean_worktree(root: Path, artifact_root: Path) -> None:
    try:
        artifact_relative = artifact_root.resolve().relative_to(root.resolve())
    except ValueError:
        artifact_relative = None
    if artifact_relative is not None:
        allowed_root = Path("proof/live-2fa")
        if (
            artifact_relative != allowed_root
            and allowed_root not in artifact_relative.parents
        ):
            raise LiveTestError("artifact_root_inside_repository_is_not_proof_live_2fa")

    status = run_checked(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"], root
    )
    for line in status.splitlines():
        if not line:
            continue
        if not line.startswith("?? ") or artifact_relative is None:
            raise LiveTestError("worktree_is_not_clean")
        changed_path = Path(line[3:])
        if (
            changed_path != artifact_relative
            and artifact_relative not in changed_path.parents
        ):
            raise LiveTestError("worktree_is_not_clean")


def required_env(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise LiveTestError("missing_required_configuration")
    return value


def secret_file(name: str, root: Path) -> tuple[Path, str]:
    path = Path(required_env(name)).expanduser()
    if not path.is_absolute():
        raise LiveTestError("secret_path_must_be_absolute")
    try:
        path = path.resolve(strict=True)
        info = path.stat()
    except OSError as error:
        raise LiveTestError("secret_file_unreadable") from error
    if not stat.S_ISREG(info.st_mode) or stat.S_IMODE(info.st_mode) & 0o077:
        raise LiveTestError("secret_file_permissions_are_not_private")
    try:
        value = path.read_text(encoding="utf-8").rstrip("\r\n")
    except (OSError, UnicodeError) as error:
        raise LiveTestError("secret_file_unreadable") from error
    if not value:
        raise LiveTestError("secret_file_is_empty")

    try:
        path.relative_to(root)
    except ValueError:
        pass
    else:
        ignored = subprocess.run(
            ["git", "check-ignore", "--quiet", str(path)],
            cwd=str(root),
            check=False,
            capture_output=True,
        )
        if ignored.returncode != 0:
            raise LiveTestError("secret_file_inside_repository_is_not_ignored")
    return path, value


def bounded_int(value: str, minimum: int, maximum: int, label: str) -> int:
    try:
        parsed = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError(f"{label} must be an integer") from error
    if not minimum <= parsed <= maximum:
        raise argparse.ArgumentTypeError(
            f"{label} must be between {minimum} and {maximum}"
        )
    return parsed


def account_scoped_lock_path(account_hash: str) -> Path:
    """Lock path for one dedicated test account, stable for this user.

    Deliberately NOT under the repository root: a per-checkout lock would let
    two worktrees or clones testing the SAME account each acquire their own
    lock and run concurrently. Their verification requests can invalidate
    each other's codes and each harness can consume the other's emails,
    breaking the documented stop-on-concurrent-run guarantee.

    Also deliberately NOT under tempfile.gettempdir(): that honours $TMPDIR,
    so two shells with different TMPDIR values would silently get different
    locks and both proceed. On Linux it also defaults to a world-writable
    /tmp, where another local user can pre-create the predictable filename
    (or a symlink at it) and deny service. The home directory is stable
    regardless of environment and private to this user.
    """
    directory = Path.home() / ".cache" / "govee2mqtt-live-2fa"
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    # mkdir's mode only applies on creation, and a pre-existing path could be
    # a symlink or someone else's. Verify rather than assume.
    info = directory.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid():
        raise LiveTestError("lock_directory_is_not_a_private_directory")
    return directory / f"{account_hash}.lock"


def verify_account_allowlist(account_email: str, expected_hash: str) -> None:
    if account_email != account_email.strip():
        raise LiveTestError("account_email_has_surrounding_whitespace")
    if not re.fullmatch(r"[0-9a-f]{64}", expected_hash):
        raise LiveTestError("account_hash_is_not_sha256")
    actual_hash = sha256_text(account_email.casefold())
    if not hmac.compare_digest(actual_hash, expected_hash):
        raise LiveTestError("account_allowlist_mismatch")


@dataclasses.dataclass(frozen=True)
class Config:
    root: Path
    expected_commit: str
    email_file: Path
    password_file: Path
    imap_host: str
    imap_port: int
    imap_mailbox: str
    imap_user: str
    imap_password: str
    expected_sender: str
    code_pattern: Pattern[str]
    email_timeout: int
    quiet_window: int
    poll_interval: int
    overall_timeout: int
    artifact_root: Path
    forbidden_artifact_values: tuple[str, ...]

    @classmethod
    def load(cls, args: argparse.Namespace, root: Path) -> "Config":
        if required_env("GOVEE_LIVE_CONFIRM") != CONFIRMATION:
            raise LiveTestError("live_confirmation_mismatch")

        expected_commit = required_env("GOVEE_LIVE_EXPECTED_COMMIT").lower()
        if not re.fullmatch(r"[0-9a-f]{40}", expected_commit):
            raise LiveTestError("expected_commit_is_not_full_sha")
        actual_commit = run_checked(["git", "rev-parse", "HEAD"], root).lower()
        if not hmac.compare_digest(actual_commit, expected_commit):
            raise LiveTestError("expected_commit_mismatch")

        artifact_root = args.artifact_root
        if not artifact_root.is_absolute():
            artifact_root = root / artifact_root
        ensure_clean_worktree(root, artifact_root)

        email_path, account_email = secret_file("GOVEE_EMAIL_FILE", root)
        password_path, account_password = secret_file("GOVEE_PASSWORD_FILE", root)
        _, imap_user = secret_file("GOVEE_LIVE_IMAP_USER_FILE", root)
        _, imap_password = secret_file("GOVEE_LIVE_IMAP_PASSWORD_FILE", root)

        account_hash = required_env("GOVEE_LIVE_ACCOUNT_SHA256").lower()
        verify_account_allowlist(account_email, account_hash)

        expected_sender = required_env("GOVEE_LIVE_IMAP_FROM").casefold()
        if (
            parseaddr(expected_sender)[1].casefold() != expected_sender
            or re.fullmatch(r"[^@\s]+@[^@\s]+", expected_sender) is None
        ):
            raise LiveTestError("imap_sender_must_be_one_bare_address")

        pattern_text = required_env("GOVEE_LIVE_CODE_PATTERN")
        if len(pattern_text) > 200:
            raise LiveTestError("code_pattern_is_too_long")
        try:
            code_pattern = re.compile(pattern_text)
        except re.error as error:
            raise LiveTestError("code_pattern_is_invalid") from error
        if "code" not in code_pattern.groupindex:
            raise LiveTestError("code_pattern_requires_named_code_group")

        if os.environ.get("GOVEE_LOG_SENSITIVE_DATA", "").strip().casefold() in {
            "1",
            "on",
            "true",
            "yes",
        }:
            raise LiveTestError("sensitive_logging_must_be_disabled")

        # Defense in depth only. The primary control is EVENT_FIELDS, which
        # whitelists the keys that may reach an artifact at all; this guard is
        # the backstop that greps written text for known secrets. Values shorter
        # than the floor are excluded because a 4-character needle matches
        # timestamps and hashes constantly, and a guard that always fires is a
        # guard that gets removed. The verification code is deliberately below
        # the floor and is never written to an artifact by any code path.
        forbidden = tuple(
            value
            for value in (account_email, account_password, imap_user, imap_password)
            if len(value) >= MIN_REDACTED_SECRET_LENGTH
        )
        return cls(
            root=root,
            expected_commit=expected_commit,
            email_file=email_path,
            password_file=password_path,
            imap_host=required_env("GOVEE_LIVE_IMAP_HOST"),
            imap_port=args.imap_port,
            imap_mailbox=os.environ.get("GOVEE_LIVE_IMAP_MAILBOX", "INBOX").strip()
            or "INBOX",
            imap_user=imap_user,
            imap_password=imap_password,
            expected_sender=expected_sender,
            code_pattern=code_pattern,
            email_timeout=args.email_timeout,
            quiet_window=args.quiet_window,
            poll_interval=args.poll_interval,
            overall_timeout=args.overall_timeout,
            artifact_root=artifact_root,
            forbidden_artifact_values=forbidden,
        )


class RunLock:
    def __init__(self, path: Path):
        self.path = path
        self.handle = None

    def __enter__(self) -> "RunLock":
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.handle = self.path.open("a+", encoding="utf-8")
        try:
            fcntl.flock(self.handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            self.handle.close()
            raise LiveTestError("another_live_test_is_running") from error
        return self

    def __exit__(self, exc_type, exc_value, traceback) -> None:
        if self.handle is not None:
            fcntl.flock(self.handle.fileno(), fcntl.LOCK_UN)
            self.handle.close()


class Evidence:
    def __init__(self, config: Config, mode: str):
        run_stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S.%fZ")
        self.run_id = f"{run_stamp}-{config.expected_commit[:12]}"
        self.path = config.artifact_root / self.run_id
        self.path.mkdir(parents=True, exist_ok=False)
        self.events_path = self.path / "events.jsonl"
        self.events = self.events_path.open("x", encoding="utf-8")
        self.started = time.monotonic()
        self.sequence = 0
        self.forbidden = config.forbidden_artifact_values
        self.network_started = False
        self.finalized = False
        self.write_json(
            "manifest.json",
            {
                "schema": 1,
                "run_id": self.run_id,
                "mode": mode,
                "commit": config.expected_commit,
                "account_allowlist_verified": True,
                "started_utc": utc_now(),
                "limits": {
                    "login_requests": MAX_LOGIN_REQUESTS,
                    "verification_requests_upper_bound": MAX_VERIFICATION_REQUESTS,
                    "govee_http_requests_upper_bound": (
                        MAX_LOGIN_REQUESTS + MAX_VERIFICATION_REQUESTS
                    ),
                    "expected_emails": EXPECTED_EMAILS,
                    "email_trigger_requests_upper_bound": MAX_VERIFICATION_REQUESTS,
                    "email_timeout_seconds": config.email_timeout,
                    "quiet_window_seconds": config.quiet_window,
                    "overall_timeout_seconds": config.overall_timeout,
                },
                "runtime": {
                    "python": platform.python_version(),
                    "platform": platform.platform(),
                },
            },
        )

    def guard(self, text: str) -> None:
        for secret in self.forbidden:
            if secret and secret in text:
                raise LiveTestError("artifact_redaction_guard_failed")

    def write_text(self, name: str, content: str) -> None:
        self.guard(content)
        destination = self.path / name
        temporary = destination.with_suffix(destination.suffix + ".tmp")
        temporary.write_text(content, encoding="utf-8")
        os.replace(temporary, destination)

    def write_json(self, name: str, value: object) -> None:
        self.write_text(name, json.dumps(value, indent=2, sort_keys=True) + "\n")

    def record(self, event: str, **fields: object) -> None:
        payload: Dict[str, object] = {
            "schema": 1,
            "sequence": self.sequence + 1,
            "time_utc": utc_now(),
            "elapsed_ms": round((time.monotonic() - self.started) * 1000),
            "event": event,
        }
        payload.update(fields)
        if not set(payload).issubset(EVENT_FIELDS):
            raise LiveTestError("event_schema_violation")
        encoded = json.dumps(payload, separators=(",", ":"), sort_keys=True)
        self.guard(encoded)
        self.events.write(encoded + "\n")
        self.events.flush()
        os.fsync(self.events.fileno())
        self.sequence += 1

    def mark_network_started(self) -> None:
        self.network_started = True

    def finalize(
        self, passed: bool, reason: str, probes: int, emails_seen: int
    ) -> None:
        if self.finalized:
            return
        self.record(
            "run_finished",
            outcome="pass" if passed else "fail",
            reason=reason,
            probe_number=probes,
            email_count=emails_seen,
        )
        self.finalized = True
        self.events.close()
        result = "PASS" if passed else "FAIL"
        summary = (
            "# Govee live 2FA test\n\n"
            f"- **Result:** {result}\n"
            f"- **Reason:** `{reason}`\n"
            f"- **Commit:** `{self.run_id.rsplit('-', 1)[-1]}` (prefix; full SHA in manifest)\n"
            f"- **Govee/IMAP contacted:** {'yes' if self.network_started else 'no'}\n"
            f"- **Login requests:** {probes}/{MAX_LOGIN_REQUESTS}\n"
            "- **Govee HTTP request upper bound:** 10 (6 login + 4 verification)\n"
            "- **Email-trigger request upper bound:** 4 (passing run observes 2 emails)\n"
            f"- **Matching emails observed:** {emails_seen}/{EXPECTED_EMAILS}\n"
            "- **Device hardware exercised:** no\n"
            "- **Secrets or message contents retained:** no\n"
        )
        self.write_text("summary.md", summary)

        suite = ET.Element(
            "testsuite",
            name="govee-live-2fa",
            tests="1",
            failures="0" if passed else "1",
            timestamp=utc_now(),
        )
        case = ET.SubElement(suite, "testcase", name="account-login-2fa-sequence")
        if not passed:
            failure = ET.SubElement(case, "failure", message=reason)
            failure.text = reason
        xml = ET.tostring(suite, encoding="unicode", xml_declaration=True) + "\n"
        self.write_text("junit.xml", xml)

        checksum_lines = []
        for artifact in sorted(self.path.iterdir()):
            if artifact.is_file() and artifact.name != "SHA256SUMS":
                checksum_lines.append(f"{sha256_file(artifact)}  {artifact.name}")
        self.write_text("SHA256SUMS", "\n".join(checksum_lines) + "\n")


@dataclasses.dataclass(frozen=True)
class ProbeResult:
    outcome: str
    status: Optional[int] = None
    code_configured: Optional[bool] = None


class Probe:
    def __init__(
        self,
        binary: Path,
        config: Config,
        cache_dir: Path,
        scratch_dir: Path,
        evidence: Evidence,
        deadline: float,
    ):
        self.binary = binary
        self.config = config
        self.cache_dir = cache_dir
        self.scratch_dir = scratch_dir
        self.evidence = evidence
        self.deadline = deadline
        self.count = 0

    def invoke(self, code: Optional[str]) -> ProbeResult:
        if self.count >= MAX_LOGIN_REQUESTS:
            raise LiveTestError("login_request_budget_exhausted")
        # Refuse to start unless the FULL probe budget remains. Clamping the
        # subprocess timeout down to whatever is left would re-create the very
        # bug PROBE_TIMEOUT_SECONDS exists to prevent: a two-request invocation
        # killed mid-flight, reported as an internal failure, after Govee may
        # already have accepted the verification request. Running out of time
        # is a real outcome and deserves its own accurate reason.
        remaining = int(self.deadline - time.monotonic())
        if remaining < PROBE_TIMEOUT_SECONDS:
            raise LiveTestError("overall_timeout_exceeded")
        self.count += 1

        child_env = {
            "PATH": os.environ.get("PATH", os.defpath),
            "TZ": "UTC",
            "GOVEE_EMAIL_FILE": str(self.config.email_file),
            "GOVEE_PASSWORD_FILE": str(self.config.password_file),
            "GOVEE_CACHE_DIR": str(self.cache_dir),
            "GOVEE_LIVE_2FA_NO_DOTENV": "1",
            "GOVEE_LOG_SENSITIVE_DATA": "0",
            "RUST_LOG": "warn",
        }

        code_path: Optional[Path] = None
        try:
            if code is not None:
                descriptor, raw_path = tempfile.mkstemp(
                    prefix="code-", dir=str(self.scratch_dir)
                )
                code_path = Path(raw_path)
                os.fchmod(descriptor, 0o600)
                with os.fdopen(descriptor, "w", encoding="utf-8") as output:
                    output.write(code)
                child_env["GOVEE_2FA_CODE_FILE"] = str(code_path)

            self.evidence.record(
                "probe_started",
                probe_number=self.count,
                code_configured=code is not None,
            )
            self.evidence.mark_network_started()
            try:
                completed = subprocess.run(
                    [str(self.binary), "undoc", "live2fa-login"],
                    cwd=str(self.scratch_dir),
                    env=child_env,
                    capture_output=True,
                    text=True,
                    timeout=PROBE_TIMEOUT_SECONDS,
                    check=False,
                )
            except (OSError, subprocess.SubprocessError) as error:
                raise LiveTestError("login_probe_execution_failed") from error
        finally:
            if code_path is not None:
                try:
                    code_path.unlink()
                except OSError as error:
                    raise LiveTestError("temporary_code_cleanup_failed") from error

        try:
            payload = json.loads(completed.stdout.strip())
        except json.JSONDecodeError as error:
            raise LiveTestError("login_probe_output_invalid") from error
        result = validate_probe_payload(payload, completed.returncode)
        self.evidence.record(
            "probe_completed",
            probe_number=self.count,
            outcome=result.outcome,
            status=result.status,
            code_configured=result.code_configured,
        )
        return result


def validate_probe_payload(payload: object, returncode: int) -> ProbeResult:
    if (
        not isinstance(payload, dict)
        or type(payload.get("schema")) is not int
        or payload.get("schema") != 1
    ):
        raise LiveTestError("login_probe_output_invalid")
    outcome = payload.get("outcome")
    if returncode != 0 or outcome == "unexpected_error":
        raise LiveTestError("login_probe_reported_redacted_failure")
    if outcome == "login_ok" and set(payload) == {"schema", "outcome"}:
        return ProbeResult(outcome="login_ok")
    if outcome == "two_factor" and set(payload) == {
        "schema",
        "outcome",
        "status",
        "code_configured",
    }:
        status = payload.get("status")
        code_configured = payload.get("code_configured")
        if (
            type(status) is int
            and status in (454, 455)
            and isinstance(code_configured, bool)
        ):
            return ProbeResult(
                outcome="two_factor", status=status, code_configured=code_configured
            )
    raise LiveTestError("login_probe_output_invalid")


@dataclasses.dataclass(frozen=True)
class MailCode:
    code: str
    message_hash: str


class VisibleTextParser(HTMLParser):
    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.parts: List[str] = []
        self.ignored_depth = 0

    def handle_starttag(self, tag: str, attrs: List[tuple[str, Optional[str]]]) -> None:
        del attrs
        if tag in {"script", "style"}:
            self.ignored_depth += 1

    def handle_endtag(self, tag: str) -> None:
        if tag in {"script", "style"} and self.ignored_depth:
            self.ignored_depth -= 1

    def handle_data(self, data: str) -> None:
        if not self.ignored_depth:
            self.parts.append(data)


def validate_code(code: str) -> str:
    if (
        not isinstance(code, str)
        or not 4 <= len(code) <= 12
        or not code.isascii()
        or not code.isalnum()
    ):
        raise LiveTestError("extracted_code_failed_safety_validation")
    return code


def mail_sender_matches(raw_header: bytes, expected_sender: str) -> bool:
    if len(raw_header) > MAX_HEADER_BYTES:
        raise LiveTestError("matching_email_header_exceeds_size_limit")
    try:
        message = email.message_from_bytes(raw_header, policy=email_policy)
    except Exception as error:
        raise LiveTestError("matching_email_could_not_be_parsed") from error
    actual_sender = parseaddr(str(message.get("From", "")))[1].casefold()
    return hmac.compare_digest(actual_sender, expected_sender)


def unseen_uids_after(
    uids: Sequence[int], baseline_uid: int, seen: set[int]
) -> List[int]:
    return sorted({uid for uid in uids if uid > baseline_uid} - seen)


def visible_message_text(message: email.message.Message) -> str:
    texts = [str(message.get("Subject", ""))]
    if len(texts[0]) > MAX_SUBJECT_CHARS:
        raise LiveTestError("matching_email_subject_exceeds_size_limit")

    for part in message.walk():
        if part.is_multipart() or part.get_content_disposition() == "attachment":
            continue
        content_type = part.get_content_type()
        if content_type not in {"text/plain", "text/html"}:
            continue
        payload = part.get_payload(decode=True)
        if not isinstance(payload, bytes):
            continue
        try:
            text = payload.decode(
                part.get_content_charset() or "utf-8", errors="replace"
            )
        except LookupError as error:
            raise LiveTestError("matching_email_charset_invalid") from error
        if content_type == "text/html":
            parser = VisibleTextParser()
            try:
                parser.feed(text)
                parser.close()
            except Exception as error:
                raise LiveTestError("matching_email_html_invalid") from error
            text = " ".join(parser.parts)
        texts.append(text)
    return "\n".join(texts)


def extract_mail_code(
    raw_message: bytes,
    expected_sender: str,
    code_pattern: Pattern[str],
    identity: str,
) -> Optional[MailCode]:
    if len(raw_message) > MAX_MESSAGE_BYTES:
        raise LiveTestError("matching_email_exceeds_size_limit")
    try:
        message = email.message_from_bytes(raw_message, policy=email_policy)
    except Exception as error:
        raise LiveTestError("matching_email_could_not_be_parsed") from error
    actual_sender = parseaddr(str(message.get("From", "")))[1].casefold()
    if not hmac.compare_digest(actual_sender, expected_sender):
        return None

    matches = {
        validate_code(match.group("code"))
        for match in code_pattern.finditer(visible_message_text(message))
    }
    if len(matches) != 1:
        raise LiveTestError("matching_email_did_not_contain_one_unique_code")
    message_id = str(message.get("Message-ID", ""))
    return MailCode(
        code=next(iter(matches)),
        message_hash=sha256_text(f"{identity}:{message_id}"),
    )


class Mailbox:
    def __init__(self, config: Config, evidence: Evidence, deadline: float):
        self.config = config
        self.evidence = evidence
        self.deadline = deadline
        self.client: Optional[imaplib.IMAP4_SSL] = None
        self.baseline_uid = 0
        self.uid_validity = "unknown"
        self.seen: set[int] = set()
        self.codes: List[MailCode] = []

    def __enter__(self) -> "Mailbox":
        self.evidence.record("mailbox_connect_started", email_count=0)
        self.evidence.mark_network_started()
        try:
            self.client = imaplib.IMAP4_SSL(
                self.config.imap_host,
                self.config.imap_port,
                ssl_context=ssl.create_default_context(),
                timeout=30,
            )
            self.client.login(self.config.imap_user, self.config.imap_password)
            status, _ = self.client.select(self.config.imap_mailbox, readonly=True)
            if status != "OK":
                raise LiveTestError("imap_mailbox_select_failed")
            uid_response = self.client.response("UIDVALIDITY")[1]
            if uid_response and uid_response[0]:
                raw_validity = uid_response[0]
                self.uid_validity = (
                    raw_validity.decode("ascii", errors="strict")
                    if isinstance(raw_validity, bytes)
                    else str(raw_validity)
                )
            self.baseline_uid = max(self._search_uids("ALL"), default=0)
        except LiveTestError:
            raise
        except Exception as error:
            raise LiveTestError("imap_connection_failed") from error
        self.evidence.record("mailbox_baseline", email_count=0)
        return self

    def __exit__(self, exc_type, exc_value, traceback) -> None:
        if self.client is not None:
            with contextlib.suppress(Exception):
                self.client.close()
            with contextlib.suppress(Exception):
                self.client.logout()

    def _search_uids(self, *criteria: str) -> List[int]:
        if self.client is None:
            raise LiveTestError("imap_not_connected")
        try:
            status, data = self.client.uid("SEARCH", None, *criteria)
        except Exception as error:
            raise LiveTestError("imap_search_failed") from error
        # `not data` catches [], but imaplib also answers [None] for some
        # servers; without AttributeError here that becomes a bare traceback and
        # main() reports the useless reason "internal_harness_error".
        if status != "OK" or not data or data[0] is None:
            raise LiveTestError("imap_search_failed")
        try:
            return [int(item) for item in data[0].split()]
        except (AttributeError, TypeError, ValueError) as error:
            raise LiveTestError("imap_search_response_invalid") from error

    def _fetch_bytes(self, uid: int, query: str) -> tuple[bytes, bytes]:
        if self.client is None:
            raise LiveTestError("imap_not_connected")
        try:
            status, data = self.client.uid("FETCH", str(uid), query)
        except Exception as error:
            raise LiveTestError("imap_fetch_failed") from error
        if status != "OK" or not data:
            raise LiveTestError("imap_fetch_failed")
        metadata = b" ".join(
            item[0]
            for item in data
            if isinstance(item, tuple) and len(item) > 1 and isinstance(item[0], bytes)
        )
        raw_message = next(
            (
                item[1]
                for item in data
                if isinstance(item, tuple)
                and len(item) > 1
                and isinstance(item[1], bytes)
            ),
            None,
        )
        if raw_message is None:
            raise LiveTestError("imap_fetch_response_invalid")
        return metadata, raw_message

    def scan(self) -> None:
        if self.client is None:
            raise LiveTestError("imap_not_connected")
        uids = self._search_uids("UID", f"{self.baseline_uid + 1}:*")
        for uid in unseen_uids_after(uids, self.baseline_uid, self.seen):
            self.seen.add(uid)
            metadata, raw_header = self._fetch_bytes(
                uid,
                "(RFC822.SIZE BODY.PEEK[HEADER.FIELDS (FROM)])",
            )
            if not mail_sender_matches(raw_header, self.config.expected_sender):
                continue
            size_match = re.search(rb"RFC822\.SIZE (\d+)", metadata)
            if size_match is None:
                raise LiveTestError("imap_fetch_response_invalid")
            if int(size_match.group(1)) > MAX_MESSAGE_BYTES:
                raise LiveTestError("matching_email_exceeds_size_limit")
            _, raw_message = self._fetch_bytes(uid, "(BODY.PEEK[])")
            mail_code = extract_mail_code(
                raw_message,
                self.config.expected_sender,
                self.config.code_pattern,
                f"{self.uid_validity}:{uid}",
            )
            if mail_code is None:
                raise LiveTestError("matching_email_sender_changed")
            self.codes.append(mail_code)
            self.evidence.record(
                "email_observed",
                email_count=len(self.codes),
                message_hash=mail_code.message_hash,
            )
            if len(self.codes) > EXPECTED_EMAILS:
                raise LiveTestError("email_budget_exceeded")

    def wait_for_total(self, expected: int) -> MailCode:
        email_deadline = time.monotonic() + self.config.email_timeout
        wait_deadline = min(self.deadline, email_deadline)
        while time.monotonic() < wait_deadline:
            self.scan()
            if len(self.codes) == expected:
                return self.codes[-1]
            if len(self.codes) > expected:
                raise LiveTestError("unexpected_matching_email_count")
            time.sleep(
                min(self.config.poll_interval, max(0, wait_deadline - time.monotonic()))
            )
        reason = (
            "overall_timeout_exceeded"
            if self.deadline < email_deadline
            else "email_wait_timed_out"
        )
        raise LiveTestError(reason)

    def assert_quiet(self, expected: int) -> None:
        quiet_deadline = time.monotonic() + self.config.quiet_window
        if quiet_deadline > self.deadline:
            raise LiveTestError("overall_timeout_exceeded")
        while time.monotonic() < quiet_deadline:
            self.scan()
            if len(self.codes) != expected:
                raise LiveTestError("unexpected_email_during_quiet_window")
            time.sleep(
                min(
                    self.config.poll_interval, max(0, quiet_deadline - time.monotonic())
                )
            )
        self.evidence.record(
            "quiet_window_passed",
            email_count=expected,
            window_seconds=self.config.quiet_window,
        )


def mutate_code(code: str) -> str:
    validate_code(code)
    last = code[-1]
    if last.isdigit() and last.isascii():
        replacement = str((int(last) + 1) % 10)
    elif "a" <= last <= "z":
        replacement = "a" if last == "z" else chr(ord(last) + 1)
    elif "A" <= last <= "Z":
        replacement = "A" if last == "Z" else chr(ord(last) + 1)
    else:
        raise LiveTestError("code_cannot_be_safely_mutated")
    mutated = code[:-1] + replacement
    if mutated == code:
        raise LiveTestError("code_mutation_failed")
    return mutated


def expect_two_factor(
    result: ProbeResult, code_configured: bool, statuses: set[int]
) -> None:
    if (
        result.outcome != "two_factor"
        or result.status not in statuses
        or result.code_configured is not code_configured
    ):
        raise LiveTestError("unexpected_2fa_probe_outcome")


def run_sequence(probe: Probe, mailbox: Mailbox) -> None:
    first = probe.invoke(None)
    expect_two_factor(first, False, {454})
    first_mail = mailbox.wait_for_total(1)

    duplicate = probe.invoke(None)
    expect_two_factor(duplicate, False, {454})
    mailbox.assert_quiet(1)

    rejected = probe.invoke(mutate_code(first_mail.code))
    expect_two_factor(rejected, True, {454, 455})
    mailbox.assert_quiet(1)

    fresh = probe.invoke(None)
    expect_two_factor(fresh, False, {454})
    second_mail = mailbox.wait_for_total(2)

    duplicate_fresh = probe.invoke(None)
    expect_two_factor(duplicate_fresh, False, {454})
    mailbox.assert_quiet(2)

    accepted = probe.invoke(second_mail.code)
    if accepted.outcome != "login_ok":
        raise LiveTestError("fresh_code_was_not_accepted")
    mailbox.assert_quiet(2)


def build_probe(config: Config, evidence: Evidence) -> Path:
    cargo = shutil.which("cargo")
    if cargo is None:
        raise LiveTestError("cargo_not_found")
    try:
        subprocess.run(
            [cargo, "build", "--locked", "--features", "live-2fa-test"],
            cwd=str(config.root),
            check=True,
            capture_output=True,
            text=True,
            timeout=600,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise LiveTestError("live_probe_build_failed") from error
    binary = config.root / "target" / "debug" / "govee"
    if not binary.is_file():
        raise LiveTestError("live_probe_binary_missing")
    evidence.write_json(
        "build.json",
        {
            "schema": 1,
            "binary": "target/debug/govee",
            "sha256": sha256_file(binary),
            "rustc": run_checked(["rustc", "--version"], config.root),
        },
    )
    evidence.record("probe_build_completed")
    return binary


def parse_args(argv: Optional[Sequence[str]] = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("preflight", "run"))
    parser.add_argument("--artifact-root", type=Path, default=Path("proof/live-2fa"))
    parser.add_argument(
        "--imap-port",
        type=lambda value: bounded_int(value, 1, 65535, "IMAP port"),
        default=993,
    )
    parser.add_argument(
        "--email-timeout",
        type=lambda value: bounded_int(value, 30, 300, "email timeout"),
        default=180,
    )
    parser.add_argument(
        "--quiet-window",
        type=lambda value: bounded_int(value, 10, 120, "quiet window"),
        default=30,
    )
    parser.add_argument(
        "--poll-interval",
        type=lambda value: bounded_int(value, 1, 10, "poll interval"),
        default=3,
    )
    parser.add_argument(
        "--overall-timeout",
        type=lambda value: bounded_int(value, 300, 900, "overall timeout"),
        default=600,
    )
    return parser.parse_args(argv)


def main(argv: Optional[Sequence[str]] = None) -> int:
    args = parse_args(argv)
    root = Path(__file__).resolve().parents[1]
    evidence: Optional[Evidence] = None
    probe_count = 0
    email_count = 0
    passed = False
    reason = "internal_harness_error"

    try:
        # Confirmation gate stays FIRST, exactly as it was before the lock
        # moved out of the checkout. It is the "I know this makes real
        # requests against a real account" acknowledgement, so it must remain
        # the error a misconfigured run reports; reading the account hash
        # ahead of it silently changed that reason.
        if required_env("GOVEE_LIVE_CONFIRM") != CONFIRMATION:
            raise LiveTestError("live_confirmation_mismatch")
        # Then the account hash -- see account_scoped_lock_path for why the
        # lock is keyed on the account rather than this checkout. Config.load
        # re-verifies this hash against the actual email, harmlessly.
        account_hash = required_env("GOVEE_LIVE_ACCOUNT_SHA256").lower()
        if not re.fullmatch(r"[0-9a-f]{64}", account_hash):
            raise LiveTestError("account_hash_is_not_sha256")
        with RunLock(account_scoped_lock_path(account_hash)):
            config = Config.load(args, root)
            evidence = Evidence(config, args.mode)
            evidence.record("configuration_validated")
            binary = build_probe(config, evidence)
            evidence.record("preflight_completed")
            if args.mode == "preflight":
                passed = True
                reason = "preflight_completed_without_network"
            else:
                deadline = time.monotonic() + config.overall_timeout
                with tempfile.TemporaryDirectory(prefix="govee-live-2fa-") as temporary:
                    scratch = Path(temporary)
                    cache_dir = scratch / "cache"
                    cache_dir.mkdir(mode=0o700)
                    probe = Probe(
                        binary, config, cache_dir, scratch, evidence, deadline
                    )
                    with Mailbox(config, evidence, deadline) as mailbox:
                        try:
                            run_sequence(probe, mailbox)
                        finally:
                            probe_count = probe.count
                            email_count = len(mailbox.codes)
                passed = True
                reason = "live_sequence_completed"
                evidence.record("live_sequence_completed", email_count=email_count)
    except LiveTestError as error:
        reason = error.reason
    except Exception:
        reason = "internal_harness_error"
    finally:
        if evidence is not None:
            evidence.finalize(passed, reason, probe_count, email_count)

    if evidence is not None:
        print(f"{reason}: {evidence.path}")
    else:
        print(reason)
    return 0 if passed else 1


if __name__ == "__main__":
    sys.exit(main())

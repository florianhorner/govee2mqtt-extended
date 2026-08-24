#!/usr/bin/env python3

import email.policy
import importlib.util
import sys
import tempfile
import unittest
from unittest import mock
from email.message import EmailMessage
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("live_2fa.py")
SPEC = importlib.util.spec_from_file_location("live_2fa", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
live_2fa = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = live_2fa
SPEC.loader.exec_module(live_2fa)


class FakeProbe:
    def __init__(self, results):
        self.results = iter(results)
        self.codes = []
        self.count = 0

    def invoke(self, code):
        self.count += 1
        if self.count > live_2fa.MAX_LOGIN_REQUESTS:
            raise live_2fa.LiveTestError("login_request_budget_exhausted")
        self.codes.append(code)
        return next(self.results)


class FakeMailbox:
    def __init__(self, first_code="1234", second_code="5678"):
        self.mails = [
            live_2fa.MailCode(first_code, "a" * 64),
            live_2fa.MailCode(second_code, "b" * 64),
        ]
        self.waits = []
        self.quiet = []

    def wait_for_total(self, expected):
        self.waits.append(expected)
        return self.mails[expected - 1]

    def assert_quiet(self, expected):
        self.quiet.append(expected)


class RecordingEvidence:
    def __init__(self):
        self.network_started = False
        self.events = []

    def mark_network_started(self):
        self.network_started = True

    def record(self, event, **fields):
        self.events.append((event, fields))


def probe(status, code_configured):
    return live_2fa.ProbeResult("two_factor", status, code_configured)


class Live2faTests(unittest.TestCase):
    def test_sequence_is_exactly_six_logins_and_two_expected_emails(self):
        runner = FakeProbe(
            [
                probe(454, False),
                probe(454, False),
                probe(455, True),
                probe(454, False),
                probe(454, False),
                live_2fa.ProbeResult("login_ok"),
            ]
        )
        mailbox = FakeMailbox()

        live_2fa.run_sequence(runner, mailbox)

        self.assertEqual(runner.count, 6)
        self.assertEqual(runner.codes.count(None), 4)
        self.assertEqual(runner.codes[0:2], [None, None])
        self.assertNotEqual(runner.codes[2], "1234")
        self.assertEqual(runner.codes[3:5], [None, None])
        self.assertEqual(runner.codes[5], "5678")
        self.assertEqual(mailbox.waits, [1, 2])
        self.assertEqual(mailbox.quiet, [1, 1, 2, 2])

    def test_sequence_stops_when_initial_login_does_not_require_2fa(self):
        runner = FakeProbe([live_2fa.ProbeResult("login_ok")])
        with self.assertRaisesRegex(
            live_2fa.LiveTestError, "unexpected_2fa_probe_outcome"
        ):
            live_2fa.run_sequence(runner, FakeMailbox())
        self.assertEqual(runner.count, 1)

    def test_code_mutation_preserves_shape(self):
        # "abcx" covers the plain lowercase increment and "abcz" the z->a
        # wraparound; without them the whole lowercase branch of mutate_code
        # was unexercised.
        for value in ("1234", "AB9Z", "token9", "abcx", "abcz"):
            mutated = live_2fa.mutate_code(value)
            self.assertNotEqual(mutated, value)
            self.assertEqual(len(mutated), len(value))
            self.assertEqual(mutated.isdigit(), value.isdigit())
        self.assertEqual(live_2fa.mutate_code("abcx"), "abcy")
        self.assertEqual(live_2fa.mutate_code("abcz"), "abca")

    def test_redaction_floor_excludes_only_short_values(self):
        # The guard deliberately skips values under MIN_REDACTED_SECRET_LENGTH.
        # Pin the boundary so a future edit cannot widen the gap unnoticed.
        floor = live_2fa.MIN_REDACTED_SECRET_LENGTH
        evidence = mock.Mock()
        evidence.forbidden = tuple(
            v for v in ("x" * floor, "y" * (floor - 1)) if len(v) >= floor
        )
        self.assertEqual(evidence.forbidden, ("x" * floor,))
        guard = live_2fa.Evidence.guard.__get__(evidence, live_2fa.Evidence)
        with self.assertRaises(live_2fa.LiveTestError):
            guard("leaking " + "x" * floor)
        guard("leaking " + "y" * (floor - 1))

    def test_code_validation_rejects_unsafe_shapes(self):
        for value in ("123", "x" * 13, "12-4", "１２３４"):
            with self.assertRaises(live_2fa.LiveTestError):
                live_2fa.validate_code(value)

    def test_account_allowlist_is_exact_and_case_insensitive(self):
        expected = live_2fa.sha256_text("test@example.com")
        live_2fa.verify_account_allowlist("Test@Example.com", expected)
        for candidate_email, digest in (
            ("other@example.com", expected),
            (" Test@example.com", expected),
            ("Test@example.com", "not-a-sha256"),
        ):
            with self.assertRaises(live_2fa.LiveTestError):
                live_2fa.verify_account_allowlist(candidate_email, digest)

    def test_account_scoped_lock_path_is_shared_across_checkouts(self):
        # Two different Path(__file__) roots (i.e. two worktrees or clones)
        # for the SAME account hash must resolve to the SAME lock path --
        # that's the whole fix. A per-checkout lock would let both proceed.
        same_account = "a" * 64
        other_account = "b" * 64
        path_a = live_2fa.account_scoped_lock_path(same_account)
        path_b = live_2fa.account_scoped_lock_path(same_account)
        self.assertEqual(path_a, path_b)
        self.assertNotEqual(path_a, live_2fa.account_scoped_lock_path(other_account))
        # Must not live inside any repository checkout.
        repo_root = Path(__file__).resolve().parents[1]
        self.assertNotIn(repo_root, path_a.parents)

    def test_probe_payload_accepts_only_the_redacted_contract(self):
        valid = {
            "schema": 1,
            "outcome": "two_factor",
            "status": 454,
            "code_configured": False,
        }
        self.assertEqual(live_2fa.validate_probe_payload(valid, 0).status, 454)
        for invalid in (
            dict(valid, email="account@example.com"),
            dict(valid, status=200),
            dict(valid, status=454.0),
            dict(valid, schema=True),
            {"schema": 1, "outcome": "unexpected_error"},
        ):
            with self.assertRaises(live_2fa.LiveTestError):
                live_2fa.validate_probe_payload(invalid, 0)

    def test_probe_subprocess_timeout_covers_both_rust_side_request_timeouts(self):
        # The Rust probe can spend up to 30s on the login request and, only
        # on a no-code 454, another 30s requesting the verification email --
        # up to 60s of Rust-side HTTP time in one invocation. The Python
        # subprocess timeout must not be shorter than that, or a slow-but-
        # legitimate run gets killed and misreported as an internal failure
        # after Govee may have already sent the email.
        self.assertGreaterEqual(live_2fa.PROBE_TIMEOUT_SECONDS, 60)

        with tempfile.TemporaryDirectory() as temporary:
            scratch = Path(temporary)
            config = type(
                "ConfigStub",
                (),
                {
                    "email_file": scratch / "email",
                    "password_file": scratch / "password",
                    "root": scratch / "repository",
                },
            )()
            evidence = RecordingEvidence()
            captured = {}

            def fake_run(command, **kwargs):
                captured.update(kwargs)
                return live_2fa.subprocess.CompletedProcess(
                    command,
                    0,
                    stdout=(
                        '{"schema":1,"outcome":"two_factor",'
                        '"status":454,"code_configured":false}'
                    ),
                    stderr="",
                )

            with mock.patch.object(live_2fa.subprocess, "run", side_effect=fake_run):
                probe_runner = live_2fa.Probe(
                    scratch / "govee",
                    config,
                    scratch / "cache",
                    scratch,
                    evidence,
                    live_2fa.time.monotonic() + 600,
                )
                probe_runner.invoke(None)

            self.assertEqual(captured["timeout"], live_2fa.PROBE_TIMEOUT_SECONDS)

    def test_probe_uses_scratch_cwd_scrubbed_env_and_ephemeral_code_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            scratch = Path(temporary)
            config = type(
                "ConfigStub",
                (),
                {
                    "email_file": scratch / "email",
                    "password_file": scratch / "password",
                    "root": scratch / "repository",
                },
            )()
            evidence = RecordingEvidence()
            captured = {}

            def fake_run(command, **kwargs):
                captured.update(kwargs)
                captured["command"] = command
                code_path = Path(kwargs["env"]["GOVEE_2FA_CODE_FILE"])
                self.assertEqual(code_path.read_text(encoding="utf-8"), "4821")
                captured["code_path"] = code_path
                return live_2fa.subprocess.CompletedProcess(
                    command,
                    0,
                    stdout=(
                        '{"schema":1,"outcome":"two_factor",'
                        '"status":455,"code_configured":true}'
                    ),
                    stderr="private upstream response",
                )

            with (
                mock.patch.dict(
                    live_2fa.os.environ,
                    {
                        "GOVEE_EMAIL": "wrong@example.com",
                        "GOVEE_API_KEY": "must-not-reach-child",
                        "GOVEE_LIVE_IMAP_PASSWORD": "must-not-reach-child",
                    },
                    clear=False,
                ),
                mock.patch.object(live_2fa.subprocess, "run", side_effect=fake_run),
            ):
                probe_runner = live_2fa.Probe(
                    scratch / "govee",
                    config,
                    scratch / "cache",
                    scratch,
                    evidence,
                    live_2fa.time.monotonic() + 60,
                )
                result = probe_runner.invoke("4821")

            self.assertEqual(result.status, 455)
            self.assertEqual(Path(captured["cwd"]), scratch)
            self.assertEqual(
                set(captured["env"]),
                {
                    "GOVEE_2FA_CODE_FILE",
                    "GOVEE_CACHE_DIR",
                    "GOVEE_EMAIL_FILE",
                    "GOVEE_LIVE_2FA_NO_DOTENV",
                    "GOVEE_LOG_SENSITIVE_DATA",
                    "GOVEE_PASSWORD_FILE",
                    "PATH",
                    "RUST_LOG",
                    "TZ",
                },
            )
            self.assertEqual(
                captured["command"], [str(scratch / "govee"), "undoc", "live2fa-login"]
            )
            self.assertFalse(captured["code_path"].exists())
            self.assertTrue(evidence.network_started)
            self.assertEqual(
                evidence.events[0],
                ("probe_started", {"probe_number": 1, "code_configured": True}),
            )
            self.assertNotIn("4821", repr(evidence.events))

    def test_mail_parser_reads_one_code_without_returning_message_text(self):
        message = EmailMessage()
        message["From"] = "Govee <support@govee.com>"
        message["To"] = "test@example.com"
        message["Subject"] = "Verification code: 4821"
        message["Message-ID"] = "<private@example.com>"
        message.set_content("Use verification code 4821 to continue.")
        pattern = live_2fa.re.compile(
            r"code(?:\s+is|:)?\s*(?P<code>[0-9]{4})", live_2fa.re.I
        )

        parsed = live_2fa.extract_mail_code(
            message.as_bytes(policy=email.policy.default),
            "support@govee.com",
            pattern,
            "1:2",
        )

        self.assertIsNotNone(parsed)
        self.assertEqual(parsed.code, "4821")
        self.assertEqual(len(parsed.message_hash), 64)
        self.assertNotIn("private", parsed.message_hash)
        self.assertTrue(
            live_2fa.mail_sender_matches(message.as_bytes(), "support@govee.com")
        )

    def test_mail_parser_ignores_other_senders(self):
        message = EmailMessage()
        message["From"] = "attacker@example.com"
        message["Subject"] = "Verification code: 4821"
        message.set_content("4821")
        pattern = live_2fa.re.compile(r"(?P<code>[0-9]{4})")
        self.assertIsNone(
            live_2fa.extract_mail_code(
                message.as_bytes(), "support@govee.com", pattern, "1:2"
            )
        )
        self.assertFalse(
            live_2fa.mail_sender_matches(message.as_bytes(), "support@govee.com")
        )

    def test_mail_parser_extracts_one_unique_code_from_html_body(self):
        message = EmailMessage()
        message["From"] = "no-reply@govee.com"
        message["Subject"] = "E-mail Verification"
        message.set_content("Use the HTML version of this message.")
        message.add_alternative(
            "<style>.code-9999 { color: red; }</style>"
            "<p>Verification code: 4821</p><p>4821</p>",
            subtype="html",
        )
        pattern = live_2fa.re.compile(r"(?P<code>[0-9]{4})")

        parsed = live_2fa.extract_mail_code(
            message.as_bytes(), "no-reply@govee.com", pattern, "1:2"
        )

        self.assertIsNotNone(parsed)
        self.assertEqual(parsed.code, "4821")

    def test_mail_parser_rejects_multiple_unique_body_codes(self):
        message = EmailMessage()
        message["From"] = "no-reply@govee.com"
        message["Subject"] = "E-mail Verification"
        message.set_content("Codes 4821 and 5932")
        pattern = live_2fa.re.compile(r"(?P<code>[0-9]{4})")
        with self.assertRaisesRegex(
            live_2fa.LiveTestError,
            "matching_email_did_not_contain_one_unique_code",
        ):
            live_2fa.extract_mail_code(
                message.as_bytes(), "no-reply@govee.com", pattern, "1:2"
            )

    def test_mail_parser_rejects_oversized_message(self):
        pattern = live_2fa.re.compile(r"(?P<code>[0-9]{4})")
        with self.assertRaisesRegex(
            live_2fa.LiveTestError, "matching_email_exceeds_size_limit"
        ):
            live_2fa.extract_mail_code(
                b"x" * (live_2fa.MAX_MESSAGE_BYTES + 1),
                "no-reply@govee.com",
                pattern,
                "1:2",
            )

    def test_mailbox_filters_reversed_empty_uid_range(self):
        self.assertEqual(live_2fa.unseen_uids_after([41, 42], 42, set()), [])
        self.assertEqual(live_2fa.unseen_uids_after([42, 43, 43, 44], 42, {43}), [44])

    def test_event_writer_rejects_secret_content_and_unknown_fields(self):
        with tempfile.TemporaryDirectory() as temporary:
            config = type(
                "ConfigStub",
                (),
                {
                    "artifact_root": Path(temporary),
                    "expected_commit": "a" * 40,
                    "email_timeout": 180,
                    "quiet_window": 30,
                    "overall_timeout": 600,
                    "forbidden_artifact_values": (
                        "secret@example.com",
                        "password-value",
                    ),
                },
            )()
            evidence = live_2fa.Evidence(config, "preflight")
            with self.assertRaises(live_2fa.LiveTestError):
                evidence.record("bad", raw="password-value")
            with self.assertRaises(live_2fa.LiveTestError):
                evidence.write_text("bad.txt", "secret@example.com")

            evidence.finalize(True, "offline_test", 0, 0)
            names = {path.name for path in evidence.path.iterdir()}
            self.assertEqual(
                names,
                {
                    "SHA256SUMS",
                    "events.jsonl",
                    "junit.xml",
                    "manifest.json",
                    "summary.md",
                },
            )
            combined = "".join(
                path.read_text(encoding="utf-8")
                for path in evidence.path.iterdir()
                if path.is_file()
            )
            self.assertNotIn("secret@example.com", combined)
            self.assertNotIn("password-value", combined)
            for line in (evidence.path / "SHA256SUMS").read_text().splitlines():
                digest, name = line.split("  ", 1)
                self.assertEqual(digest, live_2fa.sha256_file(evidence.path / name))


if __name__ == "__main__":
    unittest.main()

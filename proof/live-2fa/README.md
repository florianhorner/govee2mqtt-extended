# Live 2FA evidence bundles

Produced by `scripts/live_2fa.py`. See [`docs/LIVE_2FA_TEST.md`](../../docs/LIVE_2FA_TEST.md)
for what each file in a bundle contains and what a passing result does and does
not prove.

## Authoritative bundle

**`20260819T230631.865433Z-0f69de279e82/`** — the full six-step live sequence,
PASS, on commit `0f69de27`.

**Coverage caveat.** Code review after that run changed the login path across
several rounds, so this bundle no longer matches HEAD byte for byte. See
`## Round 3 gates` and `## Round 4` in
[`../issue-36-local-checks.log`](../issue-36-local-checks.log) for a per-change
statement of what the live run did and did not exercise. See what moved with:

```bash
git diff 0f69de279e82 HEAD --stat -- ':(exclude)proof'
```

## All bundles

| Run id | Commit | Result | What it was |
|---|---|---|---|
| `20260819T225100.260859Z-b11264d649cb` | `b11264d6` | PASS | preflight — config, worktree and build validated, no network |
| `20260819T225129.062745Z-b11264d649cb` | `b11264d6` | FAIL | `email_wait_timed_out` — found that mail arrives from `no-reply@govee.com` with a **four**-digit code in the HTML body |
| `20260819T230300.991610Z-9fffa49e5aee` | `9fffa49e` | PASS | preflight |
| `20260819T230316.106757Z-9fffa49e5aee` | `9fffa49e` | FAIL | `unexpected_email_during_quiet_window` — found that this IMAP server returns its highest UID for an empty `baseline+1:*` range |
| `20260819T230617.863047Z-0f69de279e82` | `0f69de27` | PASS | preflight |
| `20260819T230631.865433Z-0f69de279e82` | `0f69de27` | PASS | **authoritative** — full live sequence |

The two FAIL bundles are kept on purpose: each one is the record of a real
finding about the provider, and both findings have regression tests
(`test_mail_parser_extracts_one_unique_code_from_html_body`,
`test_mailbox_filters_reversed_empty_uid_range` in `scripts/test_live_2fa.py`).
They are evidence that the harness fails closed, not noise.

Totals across all six runs: 9 login requests, at most 7 verification requests,
at most 16 Govee HTTP requests, 4 delivered emails. No device or MQTT endpoint
was contacted in any run.

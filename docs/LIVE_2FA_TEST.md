# Live 2FA test

`scripts/live_2fa.py` runs the complete account-login test without accessing
devices or MQTT. It uses the production Rust login and verification code, reads
the emailed code over IMAP, and writes a redacted evidence bundle.

## Safety contract

- Use a **dedicated Govee test account and mailbox** whose no-code login returns
  status 454. Keep devices off that account when possible.
- One run makes exactly six login attempts. Given the fixed sequence, it can
  make at most four verification requests that can trigger email. A passing run
  observes exactly two matching emails; provider-side duplicates are detected
  within the observation windows but cannot be prevented.
- The runner stops on an unexpected response, an observed third matching email,
  a timeout, a commit mismatch, a dirty worktree, or a concurrent run. The
  concurrent-run lock is keyed on the account (a hash of
  `GOVEE_LIVE_ACCOUNT_SHA256`) and lives in `~/.cache/govee2mqtt-live-2fa/`
  (mode `0700`), so a second run against the same account is refused even when
  it is launched from a different worktree or clone, and `$TMPDIR` cannot move
  it. Two runs against two *different* dedicated accounts are independent and
  may proceed in parallel.
- A probe is never started unless the full per-probe budget still remains
  inside the overall timeout. Running short reports `overall_timeout_exceeded`
  rather than killing a request Govee may already have accepted.
- IMAP is opened read-only with certificate verification. The runner first
  fetches only the `From` header with `BODY.PEEK`. After the sender matches
  exactly, it fetches that message with `BODY.PEEK`. Messages over 128 KiB fail;
  attachments and script/style content are ignored. Visible text stays in
  memory, and messages are not marked read.
- Sender matching compares the `From` header only. That header is not
  authenticated, so a message forged to look like `GOVEE_LIVE_IMAP_FROM` would
  be accepted by the runner. The blast radius is one bogus code submitted to
  Govee's real login endpoint, which rejects it — no secret is exposed and no
  account is compromised — but prefer a dedicated mailbox whose provider
  enforces DMARC with a reject policy, so forged mail never lands in the INBOX
  this script polls.
- Credentials must be in private (`0600`) files. They are never passed as command
  arguments or written to evidence. Fetched message data and verification codes
  remain in memory; each submitted code also exists briefly in a `0600`
  temporary file that is removed on normal and handled-error exits.
- `GOVEE_LOG_SENSITIVE_DATA` is forced off in the Rust child process. The probe
  returns only the outcome, status 454/455, and whether a code was configured.
- The child runs from an isolated temporary directory and disables `.env`
  loading, so unrelated local Govee credentials cannot override the allowlisted
  files. It receives a minimal environment rather than inherited proxy or
  credential settings.

The upper bound is **10 Govee HTTP requests**: six login requests and up to four
verification requests. The run has a 10-minute overall timeout by default and
does not retry outside the defined sequence.

## One-time setup

Create four files outside the repository, each readable only by your user:

| File | Content |
|---|---|
| Govee email | Dedicated test-account email |
| Govee password | Dedicated test-account password |
| IMAP user | Dedicated mailbox login |
| IMAP password | Prefer an app-specific password |

Set their permissions before use:

```bash
chmod 600 /absolute/path/to/govee-email \
  /absolute/path/to/govee-password \
  /absolute/path/to/imap-user \
  /absolute/path/to/imap-password
```

Export the non-secret configuration and file paths:

```bash
export GOVEE_EMAIL_FILE=/absolute/path/to/govee-email
export GOVEE_PASSWORD_FILE=/absolute/path/to/govee-password
export GOVEE_LIVE_IMAP_USER_FILE=/absolute/path/to/imap-user
export GOVEE_LIVE_IMAP_PASSWORD_FILE=/absolute/path/to/imap-password
export GOVEE_LIVE_IMAP_HOST=imap.example.com
export GOVEE_LIVE_IMAP_FROM=no-reply@govee.com
export GOVEE_LIVE_CODE_PATTERN='(?P<code>[0-9]{4})'
export GOVEE_LIVE_CONFIRM=allow-10-requests-4-email-triggers
export GOVEE_LIVE_EXPECTED_COMMIT="$(git rev-parse HEAD)"
export GOVEE_LIVE_ACCOUNT_SHA256="$(python3 - "$GOVEE_EMAIL_FILE" <<'PY'
import hashlib
import pathlib
import sys

email = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8").strip().casefold()
print(hashlib.sha256(email.encode("utf-8")).hexdigest())
PY
)"
```

The dedicated account used for live validation received a four-digit code in
the HTML body from `no-reply@govee.com`. Change `GOVEE_LIVE_CODE_PATTERN` if
the dedicated account receives another format. The pattern must contain one
named `code` group; accepted values are 4–12 ASCII letters or digits.

## Run

Commit the implementation locally first. Both modes require the exported full
commit SHA to match `HEAD` and reject tracked or unrelated untracked changes;
earlier `proof/live-2fa/` bundles are allowed.

First validate the commit, worktree, credential files, redaction settings, and
feature-gated Rust build without connecting to Govee or IMAP:

```bash
scripts/live_2fa.py preflight
```

Then run the fully automated live sequence:

```bash
scripts/live_2fa.py run
```

The runner performs this fixed sequence:

| Step | Login input | Required result |
|---:|---|---|
| 1 | No code | 454, first email arrives |
| 2 | No code | 454, no duplicate email |
| 3 | Deliberately altered first code | 454/455, no email |
| 4 | No code | 454, fresh email arrives |
| 5 | No code | 454, no duplicate email |
| 6 | Fresh code | Login succeeds, no email |

Each no-email assertion observes a 30-second quiet window. Email arrival waits
up to three minutes. Both limits are bounded CLI options if the provider is
slower.

## Evidence

Every run that passes configuration validation gets a unique directory under
`proof/live-2fa/` containing:

- `manifest.json`: commit, mode, run id, runtime, successful account allowlist
  gate, and limits
- `build.json`: probe binary path, its SHA-256, and the `rustc` version
- `events.jsonl`: fsynced, append-only state transitions and hashed message IDs
- `summary.md`: pass/fail result and request/email counts
- `junit.xml`: machine-readable result
- `SHA256SUMS`: integrity hashes for all other evidence files

No raw server response, mailbox content, address, password, token, or
verification code is retained. A passing result proves the account flow only;
it is **not Govee hardware testing**.

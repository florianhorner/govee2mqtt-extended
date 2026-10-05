<!-- BEGIN: commit-message-standards (managed by bootstrap-repo.sh — do not hand-edit) -->
## Commit messages

Use `type(scope): subject`, for example `fix(auth): reject expired sessions`.
Allowed types are `feat fix docs style refactor test chore ci build perf revert`.
Keep the subject at most 72 characters and omit a trailing period. The title of
a pull request opened by a dependency bot (Dependabot, Renovate, pre-commit.ci)
may be longer.

For `feat` commits with >50 lines changed, add a body line beginning with
`Why:` that explains the reason for the change. This is a convention; CI does
not fail without it.

CI checks every commit of a pull request and the pull request title with
`.config/commit-lint/validate-pr.sh`. To check a message before pushing, run
`bash .config/commit-lint/commit-msg <message-file>`. Both files are generated;
do not edit them by hand.

<!-- END: commit-message-standards -->

## Public content

This repository is public. Some things never belong in it, and CI refuses them:

- Private network details: LAN addresses, hardware and device identifiers (the sample
  ids in `test-data/` are the exception), private host names, home-directory paths.
- People: e-mail addresses, other than placeholder domains used by tests and the few
  vendor and service addresses the guard lists by exact value.
- Internal process: the names of review or coding tools, their commands, review-stage
  vocabulary and status codes. Evidence in `proof/` states what ran and what it found;
  it does not narrate which tool found it.
- The maintainer's engineering backlog. It lives outside the repository; the public list
  is `ROADMAP.md`. A tracked `TODOS.md` fails CI.

`scripts/check_public_content.py --all` runs in CI and `--staged` runs in pre-commit;
`python3 -m unittest scripts/test_check_public_content.py` covers it. Pull request
bodies are linted for the same classes (`scripts/lint-patterns.sh`, read from the base
branch so a PR cannot change the gate that judges it) plus a shared editorial baseline
pinned by commit. The file scanner is the stricter of the two: it also exempts fixture values
by exact match and refuses a tracked backlog. Redact logs pasted from a
running bridge first: it prints device names, ids and LAN addresses at startup.

## Proof blocks

Every PR body must end with a `## Proof` section: one checklist line per claim,
each pointing at an artifact someone else can open. CI enforces it via
[`.github/workflows/verify-claims.yml`](.github/workflows/verify-claims.yml).

```md
## Proof
- [x] tests: proof/my-change-checks.log — `cargo test --all`, 133 passed
- [x] runtime: https://github.com/florianhorner/govee2mqtt-extended/actions/runs/<id>
- [ ] schema: n/a — no wire-format change
```

**Where artifacts live.** Commit them to `proof/` at the repo root and reference
them by repo-relative path. Not `.context/` — that is agent and workspace scratch
space, and it is gitignored.

**Accepted forms.** A repo-relative file path under `proof/`, a GitHub Actions run
URL, a gist or release-asset URL, a screenshot URL, or a bare test name. A `— what
it shows` note after the artifact is fine; the validator strips it.

**`n/a` rules.** An unchecked line must read `n/a — <reason>`; a bare `n/a` fails.
On a PR from a fork, `runtime: n/a` is rejected outright — give a real runtime
artifact or drop the `runtime:` line entirely. Never invent a URL: an absent claim
is honest, a fabricated artifact is not.

**If you edit the PR body**, note that each `verify-claims` run validates the body
as it was when that run was queued. A run queued before your edit will keep
failing on the old body; the run triggered by the edit is the one that counts.


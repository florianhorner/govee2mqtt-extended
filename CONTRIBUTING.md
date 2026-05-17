<!-- BEGIN: commit-message-standards (managed by bootstrap-repo.sh — do not hand-edit) -->
## Commit messages

This repo follows the [engineering-standards commit-message spec](https://github.com/florianhorner/engineering-standards/blob/main/specs/commit-message-spec.md). The cheat sheet below is self-sufficient — you do not need to leave the repo to write a conformant commit.

### 30-second cheat sheet

1. **Format:** `type(scope): subject` — e.g. `fix(auth): handle expired session cookie`
2. **Allowed types:** `feat fix docs style refactor test chore ci build perf revert`
3. **Subject:** ≤72 chars total, imperative mood ("fix bug" not "fixed bug"), no trailing period, no `v1.2.3` prefix
4. **Body required only when:** type is `feat` AND >50 lines changed. Body must include a `Why: <one-line>` (rule_id `WHY_REQUIRED`)
5. **Bypass:** `--no-verify` is allowed only with a `Policy-Override: <reason>` trailer (otherwise CI blocks)

### Good examples

```
fix(auth): handle expired session cookie returning undefined
```

```
docs(readme): clarify install prerequisites
```

```
feat(curve-card): add brightness scrubber with bar gauges

Why: ops team needs at-a-glance brightness state without opening editor.
Tested: e2e curve-editor + unit tests for scrubber state.
Refs: closes #67
```

### Bad examples (with the rule_id they violate)

```
Add files via upload                                 # rule_id: WEB_UI_DEFAULT
v2.10.11 feat(jamendo): country + order filters     # rule_id: VERSION_IN_SUBJECT
chore: addressed all the review comments             # rule_id: AGENT_SELF_TALK
```

```
feat(auth): add OAuth flow

florian asked me to add this                         # rule_id: OPERATOR_ATTRIBUTION (body)
```

### Body-when-required rule

A `Why:` body line is REQUIRED when **both** conditions hold:
- type is `feat`
- `git diff --shortstat` shows >50 lines changed

For all other commits the body is optional. Acceptable terse `Why:` templates:
- `Why: closes #N` (when issue body has the context)
- `Why: incident response — outage 2026-05-08T03:00Z`
- `Why: spec at <url>; see decision log section 3`

### Banned patterns — body only

| rule_id | Disallowed | Fix |
|---|---|---|
| `OPERATOR_ATTRIBUTION` | `florian asked`, `as requested`, `per request`, `per my request` | Replace with WHY: "fix X because Y" |
| `AGENT_SELF_TALK` | `addressed all`, `fix all`, `fixed all`, `cleaned up everything` | Name specific changes: "fix N+1 in Foo.query, dedupe Bar.helper" |

### Banned patterns — subject only

| rule_id | Disallowed | Fix |
|---|---|---|
| `WEB_UI_DEFAULT` | `Add files via upload`, `Update Foo.md`, `Initial commit` | Use `type(scope): subject`; describe what changed |
| `VERSION_IN_SUBJECT` | Subject starting with `v[0-9]` | Drop the version prefix; use `chore(release): 1.2.3` if needed |

### Exempt subjects (skip the format check entirely)

- Subjects starting with `Merge ` (git merge commits)
- Subjects starting with `Revert ` (`git revert`-generated)
- Subjects starting with `cherry-pick: ` (labeled cherry-picks)
- Subjects starting with `[hotfix] ` (emergency hotfix override)

### Bot allowlist

Commits authored by these identities skip the `WHY_REQUIRED` rule (subject banned-patterns still apply):

- `renovate[bot]`
- `dependabot[bot]` (this repo's `.github/dependabot.yml` sets `commit-message.prefix: "chore"` so the format check passes)
- `pre-commit-ci[bot]`
- `app/github-actions`

### Bypass policy

`git commit --no-verify` skips the local commit-msg hook. CI still validates on push. To pass CI on a sanctioned bypass:

1. Subject matches an exempt prefix (`Merge `, `Revert `, `cherry-pick: `, `[hotfix] `), OR
2. Body includes a `Policy-Override: <reason>` trailer

Example sanctioned bypass:

```bash
git commit --no-verify -m "[hotfix] fix prod outage from migration 0042" \
  -m "" \
  -m "Policy-Override: prod outage; migrating roll-forward fix; full review tomorrow"
```

The pre-push hook logs every `--no-verify` to `~/.commit-bypass.log` with the override reason.

### Where the rules live

- **Canonical spec:** https://github.com/florianhorner/engineering-standards/blob/main/specs/commit-message-spec.md
- **Vendored copy in this repo:** [`.config/commit-rules.json`](.config/commit-rules.json) — SHA-pinned snapshot consumed by the local hook, the commitlint config, and CI. Do not hand-edit.
<!-- END: commit-message-standards -->

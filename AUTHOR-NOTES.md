# Author notes

I author commits in this fork branch following [my engineering standards](https://github.com/florianhorner/engineering-standards). This file describes my contribution conventions for transparency, so reviewers understand the patterns they will see in `git log`.

## Conventions

- **Conventional Commits format.** `type(scope): subject` with `≤72` char subjects, imperative mood, no trailing period. Allowed types: `feat fix docs style refactor test chore ci build perf revert`.
- **Body when load-bearing.** A `Why: <one-line>` body is included when the change spans >50 lines or when the rationale isn't obvious from the diff. Otherwise the subject stands alone.
- **No operator-attribution self-talk.** Commits never read like `"florian asked me to fix this"` or `"addressed all comments"`. The validator blocks both patterns; messages explain WHY the change is needed and name specific edits.
- **Agent-telemetry trailers stripped on upstream-bound branches.** Metadata trailers like `Skill-Run:` and `Tool:` are stripped from commits in this fork branch when committed via my `/commit` skill, since they're agent telemetry meant for my own portfolio repos, not upstream history. Trailers preserved on fork branches: `Co-Authored-By`, `Tested`, `Refs`, `Signed-off-by`, `Policy-Override`. The transparency policy is documented at [engineering-standards](https://github.com/florianhorner/engineering-standards/blob/main/specs/commit-message-spec.md#fork-branch-behavior).

## Why this file exists

I work across multiple AI tools (Claude Code, Conductor, Codex). The standards above keep `git log` readable for humans regardless of which tool produced a given commit, and avoid leaking agent-runtime metadata into repos I don't own. If anything in the fork's history looks off, the canonical rules at the link above are the source of truth.

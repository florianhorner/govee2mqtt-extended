#!/usr/bin/env bash
# Project-specific editorial patterns for pull request bodies.
#
# Sourced by the shared body lint (florianhorner/gh-workflows body-lint.yml)
# after its baseline; a repository may only ADD patterns. The workflow reads
# this file from the BASE branch, so a pull request cannot change the rules
# that judge it. The same classes are enforced on tracked files by
# scripts/check_public_content.py; keep the two in step.
#
# POSIX ERE that behaves identically under BSD grep (macOS) and GNU grep (CI):
# no lookaround, no \d, no (?:...), no non-greedy quantifiers.
# shellcheck disable=SC2034
LINT_PATTERNS=(
  # Internal review and tooling narration.
  '[Oo]utside [Vv]oice'
  '[Aa]dversarial[ -]([Rr]eview|[Pp]ass|[Cc]hallenge)'
  '[Pp]re-([Ss]hip|[Ll]anding)([^[:alnum:]]|$)'
  '(^|[[:space:]`(])/(ship|review|retro|autoplan|office-hours|codex|qa|investigate|plan-[a-z]+-review)([^[:alnum:]_-]|$)'
  '[Cc]odex'
  '[Gg]stack'
  '[Ss]ubagent'
  'DONE_WITH_CONCERNS|NEEDS_CONTEXT'
  '[Ss]cope [Dd]iscipline'
  '(squad|ranked) P[0-3]'
  'Florian (sends|asked|approves|decides|clicks|opens|merges|reviews)'
  # Local machine paths.
  '\.context/'
  '\.(claude|gstack|codex|mempalace|conductor)/'
  '/Users/[A-Za-z0-9_]'
  '/home/[A-Za-z0-9_]'
  '(^|[^0-9A-Fa-f:])f[cd][0-9A-Fa-f]{2}:[0-9A-Fa-f:]{2,}'
  # Private network and hardware identifiers.
  '192\.168\.[0-9]{1,3}\.[0-9]{1,3}'
  '(^|[^0-9.])172\.(1[6-9]|2[0-9]|3[01])\.[0-9]{1,3}\.[0-9]{1,3}'
  '(^|[^0-9.])100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])\.[0-9]{1,3}\.[0-9]{1,3}'
  '[Hh][Aa]-[Gg][Rr][Ee][Ee][Nn]|\.ts\.net'
  '([0-9A-Fa-f]{2}:){7}[0-9A-Fa-f]{2}'
  '(^|[^0-9A-Fa-f:])([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}([^0-9A-Fa-f:]|$)'
  # Compact Govee device ids are upper-case hex; lower-case 16-hex strings are
  # cargo test binary hashes and stay allowed, as in the file scanner.
  '(^|[^[:alnum:]])[0-9A-F]{16}([^[:alnum:]]|$)'
  # E-mail addresses. Handles like @peas have no domain and do not match.
  '[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}'
)

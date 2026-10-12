# Trusted commit policy bootstrap validation

Validated on 2026-10-12. Generated source revision: `dcc7310891aa9f295fdbcc02340db6e0eaf6bf7a`.

## Local results

- Validator suite: 82 tests passed, including 20 controller tests and the 61-case message corpus exercised by the kit tests.
- Installer suite: 30 tests passed. The regression for an old three-file manifest verifies four current generated entries, correct SHA256 values, and an unchanged retained caller.
- Workflow syntax: `actionlint` passed for the trusted workflow.

The controller tests use real temporary Git repositories and the real trusted message checker. API responses and network fetch destinations are substituted locally. Invalid commits and titles fail even when the PR replaces its checker and workflow with no-op or executable payloads. Tests also cover credential isolation, missing or modified trusted policy, fetch failures, stale PR snapshots, bot-title allowances, and timeout failures.

The existing Rust dependency change is unchanged from the previous PR revision. This validation does not claim new hosted Rust or container results.

## Installation boundary

The existing pinned workflow and its policy files remain active during bootstrap. The new `pull_request_target` workflow checks out trusted default-branch policy; PR history enters a temporary bare repository as Git objects only.

Actual hosted event admission, PR-head check association, and deliberate fail/pass preview runs must be verified after the workflow reaches the default branch. Activation also requires confirmation that required-check settings permit the transition. The old caller and policy copies are removed only in the activation change.

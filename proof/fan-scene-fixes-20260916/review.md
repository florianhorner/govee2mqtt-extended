# Focused source review

Result: **complete, no concrete additional findings** in the reviewed fan-control paths. Read-only supporting review; no tests, source edits, live API requests or external writes were performed in this phase.

- `fan_power` is reached by both the dedicated fan power handler and percentage-zero while their Coordinator remains alive.
- The helper reads the vendor state instead of its stale device snapshot. Metadata, client and readback failures prevent actuator writes.
- Nightlight restoration is attempted even when the power response failed; errors from either operation remain errors.
- Discovery, the registered route inventory, live route bindings and per-device dispatch identity agree.
- Generic master-power and unverified other-model paths retain their existing behavior.
- The regression fixture models master-power coupling and exercises actual handlers and HTTP requests rather than duplicating the production decision logic.

Scope limits: this is a focused source review, not a full release/security review or hardware confirmation. Separate HTTP acknowledgements do not prove physical ordering or zero flicker. The source patch is retained alongside this note.

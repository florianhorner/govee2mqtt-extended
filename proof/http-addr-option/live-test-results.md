# Live ingress test — results

Run against this branch on a real Home Assistant instance, 2026-09-23,
12:41–12:56 BST. HA 2026.9.3, amd64, add-on built locally by Supervisor from
the source in the build context (`live-test-scaffold/`).

Addresses are redacted to `<bridge>` and the probe host omitted: this
repository refuses private network addresses, and the Home Assistant bridge
gateway is in one of the refused ranges. `<bridge>` is the address Supervisor
reports as the add-on's `ip_address` and proxies ingress from.

## Identification — is this build the branch?

1. `Starting service. version ingress-test-97d216e`, matching the `.tag`
   written at copy time. Identifies the binary.
2. `Web UI bound to <bridge> (ingress only; set http_addr to 0.0.0.0 for
   direct access)`, then `http server addr is <bridge>:8056`. Identifies
   `run.sh`.
3. `index.html` fetched through ingress contains `href="style.css"` and
   `src="components/devices.js"`, no leading slash. Identifies the assets.

## Checks

Probed from a single-homed host on the device network, with no gateway in the
path. Baseline with the published add-on: 8056 answered 303, 8123 answered
200.

| # | Result |
|---|--------|
| a | Ingress `GET <prefix>/` → 303 with a **relative** `Location: assets/index.html`; `<prefix>/assets/index.html` → 200; `<prefix>/api/devices` → 200 listing both devices. `API = '../api'` resolves to exactly that URL from the served page. |
| b | `http_addr` unset: port 8056 **connection refused** from the device network; 8123 answered 200 as the control. |
| c | `http_addr` = `0.0.0.0`: `http server addr is 0.0.0.0:8056`; 8056 answered 303 from the device network again. |
| d | `http_addr` = `127.0.0.1`: ingress `<prefix>/api/devices` → **502 Bad Gateway**, the same URL that returned 200 in (a); 8056 refused from the device network. |

Check (b) is the one that matters, and 8123 answering proves the probe could
have seen an open port. Check (d) is the must-fail for (a): loopback breaks
ingress, exactly as the option description and `docs/CONFIG.md` warn.

## What this does not cover

- **Rendering.** The page and its API were fetched and inspected; no browser
  rendered it. A JavaScript error after load would not have been caught.
- **The fail-open path.** Detection returned a value on this base image, so
  the branch where `run.sh` cannot determine the address — and warns, and
  leaves the bind at `0.0.0.0` — was never exercised. Untested, not disproven.
- **The shipped build path.** `addon/Dockerfile` is unchanged and was not used
  here; it is still covered only by CI's `test-addon` job.

## Noted during the run, out of scope for this PR

Supervisor warns that `build.yaml` is deprecated in favour of build parameters
in the Dockerfile, and that the `addon_config` map type is legacy in favour of
`app_config`. Both apply to the existing add-on, not to this change.

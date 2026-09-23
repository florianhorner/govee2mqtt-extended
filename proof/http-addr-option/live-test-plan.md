# Live ingress test plan

The ingress change cannot be verified on a developer machine: it depends on
Supervisor proxying to a `host_network` add-on. This is the procedure, the
expected results, and what each check would show if it failed.

## Build and install

The add-on builds locally from `addon/`, so nothing has to be published.

1. Copy this branch's `addon/` directory to the Home Assistant host as
   `/addons/govee2mqtt-ingress-test/` (Settings → Add-ons → Add-on store →
   three-dot menu → "Check for updates" makes Supervisor rescan local add-ons).
   `addon/Dockerfile` takes the binary from `ghcr.io/florianhorner/govee2mqtt:latest`
   by default, so the test exercises this branch's `run.sh`, `config.yaml` and
   translations against the published binary. To test this branch's Rust
   changes as well, build and push the standalone image first and pass
   `GOVEE_IMAGE`; the UI files and the redirect live in that image.
2. Stop the installed `govee2mqtt` add-on first: both bind port 8056, and the
   second to start will fail. Stopping it removes the bridge's device control,
   so pick a window where no light automation depends on it.
3. Install and start the local add-on, leaving `http_addr` unset.

## Checks

| # | Action | Expect | A failure would mean |
|---|--------|--------|----------------------|
| a | Open the add-on page, click **Open Web UI** | The UI loads through ingress and the device list populates | Relative paths or the ingress port are wrong |
| b | From a single-homed host on the device network, `nmap -Pn -p 8056,8123 <ha-address>` | 8056 closed, **8123 open** | The bind did not restrict, or (if 8123 is also closed) the probe never reached the host |
| c | Set `http_addr` to `0.0.0.0`, restart, repeat (b) | 8056 open again | The escape hatch does not work |
| d | Set `http_addr` to `127.0.0.1`, restart, open the UI | The UI does **not** load | The documented warning is wrong |
| — | After (d), set `http_addr` back to blank and restart | UI loads through ingress again | — |

Check (b) is the one that matters: it is the only one that can show the
listener is actually restricted, and 8123 is the control that proves the
probe could have seen an open port. Run it from a host with one leg on that
network, not from a machine that also has another route to Home Assistant.

## Afterwards

Uninstall the local add-on and start the original one again. Confirm the
lights respond before leaving it.

## Status

**Run and passed** — see `live-test-results.md` for what was measured.

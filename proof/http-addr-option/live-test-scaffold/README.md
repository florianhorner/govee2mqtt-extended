# Live ingress test scaffold — NOT SHIPPED

This directory is test scaffolding, not part of the add-on. `Dockerfile.test`
exists so the live test builds **this branch** — binary, assets and `run.sh` —
on the Home Assistant host, with nothing pushed to a registry.

## Why the ordinary local build is not enough

`addon/Dockerfile` takes the binary and the assets out of
`ghcr.io/florianhorner/govee2mqtt:latest`. A local build of this branch would
therefore test this branch's `run.sh` and `config.yaml` against the published
UI, which still uses rooted paths. Check (a) would fail and prove nothing
about this branch. `Dockerfile.test` builds from the source in the context
instead.

## Assembling the test add-on

On the Home Assistant host, in `/addons/govee2mqtt-ingress-test/`:

| From this branch | To | Why |
|---|---|---|
| `addon/run.sh` | `run.sh` | the change under test |
| `addon/translations/` | `translations/` | option description |
| `addon/config.yaml` | `config.yaml` | with the three edits below |
| `addon/build.yaml` | `build.yaml` | supplies `BUILD_FROM`; without it the runtime stage has no base |
| `proof/http-addr-option/live-test-scaffold/Dockerfile.test` | `Dockerfile` | build from source |
| `src/`, `assets/`, `Cargo.toml`, `Cargo.lock`, `build.rs`, `AmazonRootCA1.pem` | same names at the top level | the build context |

Three edits to the copied `config.yaml`:

1. **Delete the `image:` line.** With it, Supervisor pulls the published image
   and never builds. This is the one that silently invalidates the test.
2. `name: Govee2MQTT Ingress Test` and `version: "ingress-test-1"`, so it does
   not collide with the installed add-on in the UI.
3. Leave `ingress: true`, `ingress_port: 8056` and `host_network: true` exactly
   as they are — they are what is being tested.

Then write a `.tag` file at the top level, derived at copy time rather than
copied from any document — the branch HEAD moves whenever a commit is
rebuilt:

```sh
echo "ingress-test-$(git rev-parse --short HEAD)" > /addons/govee2mqtt-ingress-test/.tag
```

`build.rs` reads `.tag` when there is no git metadata, and that string becomes
the version the bridge logs at startup. The identification check below
compares the log line against the value you just wrote, so it stays correct
whatever HEAD is. Without a `.tag` the version reads as the crate version and
you cannot tell the builds apart.

Supervisor picks the add-on up from Settings → Add-ons → Add-on store →
⋮ → "Check for updates", and builds it on install.

## Positive identification — is this the branch build?

Check all three; any one alone can mislead.

1. **Version line.** The add-on log's first line reads
   `Starting service. version <the string you put in .tag>`. The published
   image prints a date-and-SHA release tag instead. This identifies the
   **binary**.
2. **Bind line.** With `http_addr` unset, the log carries
   `Web UI bound to <address> (ingress only; set http_addr to 0.0.0.0 for direct access)`.
   Only this branch's `run.sh` prints it. This identifies the **add-on wrapper**.
3. **UI source.** View source on the loaded page: `<link href="style.css">` and
   `src="components/devices.js"` with no leading slash. The published UI has
   `/assets/...`. This identifies the **assets**, which is what check (a) turns
   on.

If (1) or (3) shows the published form, stop: the build fell back to the
published image and check (a) is meaningless.

## Cost, estimated

Not measured — these are estimates, so watch the first build rather than trust
them.

- **Time:** a cold release build of this crate compiles vendored OpenSSL and
  mosquitto-rs from source. Roughly 6–12 minutes on an x86 mini-PC, 15–35 on
  ARM. Supervisor shows no progress bar; the add-on simply takes a long time to
  install.
- **Disk:** the `rust:1-bookworm` image is about 1.5 GB unpacked, the release
  `target/` a few GB, and the resulting add-on image a few hundred MB on top of
  the Debian base. Budget **5–8 GB peak**. Against ~30 GB free that is
  comfortable.
- **Afterwards:** uninstalling the test add-on removes its image, but the
  builder layers stay in Docker's cache. Reclaiming those is a separate
  cleanup, and worth doing if disk gets tight.

## Not covered by this scaffold

The real `addon/Dockerfile` is unchanged and untested by this procedure, so it
still needs CI's `test-addon` job. This proves the runtime behaviour, not the
shipped build path.

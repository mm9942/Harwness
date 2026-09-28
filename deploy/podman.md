# Containerized harw (podman + quadlet)

Run the harw control plane in a rootless podman container, supervised by
systemd — same behavior as the plain `deploy/systemd/harw-control.service`
unit, with the container itself acting as the sandbox.

## Files

| File | Purpose |
| --- | --- |
| `deploy/Containerfile` | Debian-slim image with the release `harw` binary, bubblewrap, dbus |
| `deploy/quadlet/harw-control.container` | Quadlet user unit → `systemctl --user start harw-control` |
| `deploy/systemd/` (unchanged) | Host-side systemd units, still the reference for hardening |

## Build

```sh
# from the repo root, after a release build exists
cargo build --release -p harw
podman build -t harw:local -f deploy/Containerfile .
```

## Install (per user, rootless)

```sh
mkdir -p ~/.config/containers/systemd
cp deploy/quadlet/harw-control.container ~/.config/containers/systemd/

# secrets are mounted, never copied
podman secret create harw-telegram ~/.harw/secrets/telegram.env

systemctl --user daemon-reload
systemctl --user start harw-control
```

Quadlet regenerates `harw-control.service` on every `daemon-reload`; edits
go to the `.container` file, never to the generated unit.

## Verify

```sh
systemctl --user status harw-control
podman ps --format '{{.Names}} {{.Status}}'
curl -sS -o /dev/null -w '%{http_code}\n' http://127.0.0.1:1337/mcp   # 401 without token = alive
```

## Why bubblewrap still works inside the container

harw's own tool sandbox (`harw-job-executor-bwrap`) needs unprivileged
user/mount namespaces. Rootless podman provides exactly those by default,
so nested bubblewrap runs without `--privileged` and without loosening
seccomp — the same property the host unit relies on (see the comment on
`RestrictNamespaces` in `deploy/systemd/harw-control.service`).

## Scope

This ships the control plane only. `harw-auth-hub`, `harw-netsec`,
`harw-warden`, `harw-sentinel` and the BPF probes stay on their host
systemd units; moving them into the same pod is a follow-up step (they
share `/run/harw/infra` sockets today, which a pod can provide later).

# Docker, Podman and frequently used CLIs as typed agent tools

> Status: research / proposal · Date: 2026-10-01 · Baseline: `main`@`9e639f5`
> plus open PRs #74, #88–#91 (read, not merged).
>
> Scope: the **agent-facing** tool surface. The **executor-side** design
> (containers as job backends, placement, DoD sensor) already exists in
> `docs/planning/67-containers/README.md` and is not repeated here; this note
> builds on it and says where the two meet.
>
> Implementation: step 1 (the pure policy core) exists as `harw-tool-container`
> (48 tests; `--init` is deliberately not passed, it needs an init binary in
> every image). Everything below that crate does not cover is still proposal.
>
> Evidence rule: every statement about our code cites a file; every statement
> about Docker/Podman/cross cites the upstream page in §11. Items I could not
> verify are listed in §9 and marked **(unverified)** where they appear.

## 0. Summary

1. **The agent cannot use containers today, by design.** The bwrap sandbox
   withholds `/run` because "a read-only bind of `/run` would still expose
   host Unix sockets (e.g. a container daemon socket)"
   (`harw-job-executor-bwrap/src/executor.rs:10-24`). So `docker …` or
   `podman …` inside `shell.exec` fails, the denial classifier points the
   model at host-mode escalation (`harw-tool-shell/src/host_escalation.rs`),
   and host mode is a broad grant for a narrow need.
2. **Nothing in the registry covers the software we use most.** The
   capability catalog has no `container`, `cargo`, `git`, `gh`, `systemd`
   family (`harw-registry-defaults/src/capability_catalog.rs`; families
   present: gateway, agents, skills, fs, browser, matrix, job, deps, web,
   explore, latex, process, …). Everything else goes through `shell.exec`.
3. **Recommendation:** one `container.*` family with a **hardened-by-default
   argv builder** (not a socket client) as the agent surface, **rootless
   Podman as the default engine**, Docker accepted only when rootless, and
   **recipes** (named profiles and image aliases) so the common call is one
   short, safe call. The same pattern (typed args → argv vector → no shell →
   policy core → job-owned process → redacted, bounded output) is then
   reused for `cargo.*`, `git.*`, `gh.*`, `service.*` (systemd user units).
4. **PR #74's Podman executor does not yet meet the bar in this note** (no
   image in the argv, no `--`, unenforced flags reported as enforced).
   §5.3 gives the exact argv a correct plan produces and §6 maps the
   differences.

## 1. Why: the evidence from our own code

| Observation | Source |
|---|---|
| Sandboxed tools never see `/run`, `/sys`, host sockets | `harw-job-executor-bwrap/src/executor.rs` (`WITHHELD_TREES`) |
| A model that needs a container must ask for host mode | `harw-tool-shell/src/host_escalation.rs` (`HostEscalation`, `denial_hint`) |
| Only the TUI can grant host mode; everywhere else it fails closed | same file, `HOST_MODE_REQUIRES_TUI_MSG` |
| Registry families: no container / cargo / git / gh / systemd | `capability_catalog.rs` rows |
| `harw-cli` already wraps `gh` correctly (argv only, no shell, size cap, read-only default) | `harw-cli/src/pr_review.rs:1-40` |
| A cargo sandbox profile with modes exists, but only as a launcher input | `harw-sandbox/src/cargo.rs` (`Inspect` / `BuildOffline` / `Fetch`) |
| `.docker` is a credential dir; Podman's auth file is not | `harw-runtime/src/auto_classifier.rs:179`; no match for `containers/auth.json` or `*.sock` in the repo |
| We already ship a rootless Podman + Quadlet unit | `packaging/podman/` |
| `cross` (Docker/Podman-backed) is how aarch64 release builds work | `Cross.toml`, `.github/workflows/release.yml` |
| Containers as job executors are designed, not built | `docs/planning/67-containers/`, PR #74 |

Consequence: the model either improvises (shell strings, host-mode
escalation) or gives up. Both are worse than a small typed surface.

## 2. Docker vs Podman: facts that decide the design

| Topic | Docker | Podman |
|---|---|---|
| Architecture | Client talks to a daemon (`dockerd`) over a socket | Daemonless by default; the CLI runs containers itself. A REST service (`podman system service`) is optional |
| API | Engine API, latest documented 1.56 (Engine 29.8); local client here is 29.6.2 / API 1.55. Version in the path (`/v1.56/…`) or `DOCKER_API_VERSION` (which disables negotiation) | Two layers: Docker-compat (documented as v1.40) and native "libpod". The server accepts unsupported versions |
| Default socket | `/var/run/docker.sock` (rootful); rootless `unix:///run/user/<uid>/docker.sock` | Rootless `$XDG_RUNTIME_DIR/podman/podman.sock`; rootful `/run/podman/podman.sock` |
| Socket = root? | **Yes**: keys/socket access "giving them root access to the machine hosting the daemon" | Rootful socket yes. Docs: the API "grants full access to all Podman functionality" with "no ability to limit or audit this access" |
| Rootless | Supported; needs `newuidmap`/`newgidmap` and ≥65,536 subuid/subgid | Default way to run; same subuid/subgid requirement; limits in §2.1 |
| Remote | `docker context` over ssh or TLS; remote user needs socket access | `--remote`, `--url ssh://user@host/run/user/<uid>/podman/podman.sock`, `--connection`, `--identity`; `tcp://` is unencrypted |
| Env that redirects the engine | `DOCKER_HOST`, `DOCKER_CONTEXT`, `DOCKER_API_VERSION` | `CONTAINER_HOST`, `CONTAINER_CONNECTION`, `CONTAINER_SSHKEY` |
| Registry credentials | `~/.docker/config.json` | `${XDG_RUNTIME_DIR}/containers/auth.json` (not persistent across reboot), `REGISTRY_AUTH_FILE`, fallback to `~/.docker/config.json`; base64, **not encrypted** |
| Supervision | `restart:` policies, compose | Quadlet (`.container/.pod/.network/.volume/.kube/.build/.image` → generated systemd units); `podman auto-update` with rollback |
| Compose | `docker compose` | `podman compose` is "a thin wrapper around an external compose provider" (docker-compose first, then podman-compose) and warns it runs an external command |
| Image trust | Docker Content Trust (not researched) | `containers-policy.json`: default `reject`, per-scope `signedBy` / `sigstoreSigned` |
| Stream framing (logs/attach, no TTY) | multiplexed: 8-byte header `[type,0,0,0,size(u32 BE)]`, type 1=stdout 2=stderr; with TTY the stream is raw | CLI users never see this; relevant only if we speak the REST API |
| Error shape | `{"message": "…"}` (`ErrorResponse`) | CLI: stderr + exit code |

### 2.1 Rootless Podman limits that will surface as tool errors

From Podman's `rootless.md`:
- ports below 1024 cannot be bound (sysctl `net.ipv4.ip_unprivileged_port_start`);
- missing `/etc/subuid`/`/etc/subgid` entries make commands fail;
- no cgroups v1 resource limits;
- images are per-user; no NFS home directories; writable home without
  `noexec`/`nodev`; only overlay/VFS storage;
- no checkpoint/restore; no device nodes;
- default rootless networking is **pasta**; `rootlessport` does not preserve
  client IPs.

The `container.engine` tool (§4) must report these as structured reasons
("rootless, subuid ok, cgroup v2, limits enforceable") so the model does not
learn them by failing.

### 2.2 What the default flags really are (Podman `run`)

Verified from the `podman run` page:
- `--pull` default is `missing` (so a tag silently pulls); values `always|missing|never|newer`.
- `--cap-drop` "removes capabilities from the default set"; `CAP_SYS_ADMIN`
  and `CAP_SYS_PTRACE` are called out as dangerous members of the default
  set. **Rootless does not mean "no capabilities".**
- `--security-opt no-new-privileges` is opt-in.
- `--read-only` default false; `--read-only-tmpfs` default true (so `/tmp`,
  `/run`, `/dev`, `/dev/shm`, `/var/tmp` stay writable under `--read-only`).
- `--pids-limit` default 2048.
- `--network`: `none`, `pasta` (rootless default), `bridge` (rootful default).
- `--timeout` kills the container via conmon after N seconds.
- `--cidfile` writes the container ID; `--replace` replaces a same-named container.

## 3. What we use and what the plans already decide

- **Control plane in rootless Podman + Quadlet**:
  `packaging/podman/quadlet/harw-control.container` already sets
  `NoNewPrivileges=true`, `DropCapability=all`, `PidsLimit=256`, `Memory=2g`,
  loopback-only `PublishPort`, secrets via `podman secret`. This is the
  reference for what "hardened" means for a long-running service.
- **Podman as job executor** (#74): `--network=none`, `--userns=keep-id`,
  `--memory`, optional SSH remote to the Pi worker.
- **`cross`**: supports Docker (≥20.10) and Podman (≥3.4.0); with both
  installed it defaults to `docker`; `CROSS_CONTAINER_ENGINE` selects. On Linux
  Docker needs the docker group or rootless Docker. Relevant because a
  `cargo.cross_build` tool must pick the engine explicitly, not by accident.
- **67-containers decisions to inherit**: rootless default, rootful socket
  refused unless `allow_rootful_socket=true`, digest-only image refs,
  enforcement from engine **read-back** not from the request, labels
  `harw.owner/work_id/attempt/lease_epoch/tenant/profile`, act on immutable
  container ID never on a name, bounded log/API bodies.

## 4. Proposed agent surface: `container.*`

### 4.1 Implementation choice: CLI argv builder first

| | A. Build an argv for `podman`/`docker` (recommended v1) | B. REST client over a Unix socket |
|---|---|---|
| Audit surface | The argv we construct; testable as a pure function (like `tunnel` policy core) | Every endpoint and JSON field |
| Exposure | The agent never gets a socket; no service needs to run | A socket exists; Podman warns the API cannot be limited or audited |
| Dependencies | None new (std `Command`, as in `pr_review.rs`) | hyper over UDS (already in tree), plus our own multiplexed-stream parser |
| Output | CLI `--format json` (**unverified for every subcommand**, §9) | JSON, but Podman libpod and Docker-compat differ |
| Fits | Agent tools, `harw worker`, Pi remote via `--remote` | Job executor lifecycle (reattach, events) in ring J per 67-containers |

Use A for tools; keep B for the executor design that already exists. They
share the vocabulary crate proposed there (`harw-container-model`).

### 4.2 Tool set

| Tool | Class (`CapabilityClass`) | Notes |
|---|---|---|
| `container.engine` | Meta | Which engine, rootless?, version/API, subuid, cgroup, image policy, **what is allowed here and why not**. Always call first; cheap |
| `container.images` | Meta | List local images with digest; no registry access |
| `container.ps` | Meta | Only harw-labelled instances by default; `all=true` lists others read-only |
| `container.inspect` | Meta | By ID; redacts env values |
| `container.logs` | Meta | Bounded bytes/lines/time; stdout/stderr separated |
| `container.run` | Host | Foreground, bounded, `--rm`; the only way a command runs in a container (§5) |
| `container.stop` / `container.rm` | Host | Only instances whose labels match this session's owner, resolved to the immutable ID first |
| `container.pull` | Network | Digest reference only; goes through egress policy; verifies `policy.json` |
| `container.build` | Host + Network | v2, approval every time: `RUN` steps execute arbitrary code, network default `none` |
| `container.exec` | Host | v2: only into harw-owned running instances |

Not in v1: compose (it delegates to an external provider and runs commands),
`--privileged`, host network/pid/ipc, device passthrough, mounting any socket,
`docker save/load/export`, registry login (credentials flow in as a secret
reference, never as arguments).

### 4.3 New permission, not `ExecuteProcess`

`Permission` has seven values (`harw-authority/src/lib.rs:29`). Reusing
`ExecuteProcess` would let any shell-capable role start containers. Follow the
`ReadCargoRegistry` precedent: add one fixed permission (working name
`ManageContainers`), granted per role, whose meaning is a fixed policy and never
a caller-chosen path or engine. Permission is checked before arguments are
deserialized (the `#[harw_macros::tool]` prologue already does this).

### 4.4 Engine selection (fail closed)

1. Config names the engine and binary path (`/usr/bin/podman`; no `PATH` search for the engine).
2. Default `podman` rootless. Docker is accepted only if `docker info` reports a
   rootless engine **(verify exact field, §9)**; docker group / rootful socket → refuse with a
   message that names the reason.
3. **Scrub the environment**: `env_clear()` then an allowlist (`HOME`,
   `XDG_RUNTIME_DIR`, fixed `PATH`; `SSH_AUTH_SOCK` only in remote mode).
   This drops `DOCKER_HOST`, `DOCKER_CONTEXT`, `CONTAINER_HOST`,
   `CONTAINER_CONNECTION`, `REGISTRY_AUTH_FILE`, `DOCKER_CONFIG`,
   `CONTAINERS_CONF` — any of which could redirect us to a rootful or remote engine.
4. Remote (Pi): explicit `connection` name from config, never a model string;
   `ssh://` only; host key pinned. `tcp://` is refused (unencrypted, per the
   `--remote` page).
5. If both engines exist, config decides. Never auto-pick (`cross` picks
   Docker silently; we must not copy that).

## 5. `container.run`: one call, safe defaults

### 5.1 Arguments the model can set

```
image      alias from the image catalog (e.g. "rust") — never a free-form ref
profile    "hermetic" | "build"            (default "hermetic")
command    argv array                      (no shell string)
workdir    relative path under /workspace
env        map, keys allow-listed, values never logged
timeout_s  clamped to the profile maximum
```

The model cannot set: mounts, network, capabilities, user, privileged,
security options, pull policy, engine, host, labels. Those come from the
profile (trusted config), exactly as `CargoSandboxProfile` is "a trusted
launcher input and not a capability a tool call can supply itself".

### 5.2 Profiles ("make it super easy")

| Profile | Workspace | Network | Extra | Typical use |
|---|---|---|---|---|
| `hermetic` | read-only | none | read-only rootfs, tmpfs `/tmp` | run a tool, lint, inspect |
| `build` | read-write | none | project-scoped cache volume (like `CargoExecutionMode::BuildOffline`) | compile/test in a pinned toolchain image |
| `fetch` (later) | read-write | proxy-only via the egress relay | approval | dependency fetch; matches `CargoExecutionMode::Fetch` |

An **image catalog** in config maps short names to digests, e.g.
`rust = "docker.io/library/rust@sha256:…"`. A tag is refused (fail closed), so
the agent writes `image: "rust"` and never sees or invents a digest.

### 5.3 The argv a correct plan produces (Podman)

```
podman run
  --rm --pull=never
  --name=harw-<session>-<n>  --cidfile=<fresh path under the job dir>
  --label=harw.owner=<runner> --label=harw.work_id=<id> --label=harw.profile=<p>
  --network=none
  --read-only                     # --read-only-tmpfs defaults to true
  --cap-drop=all
  --security-opt=no-new-privileges
  --pids-limit=256 --memory=<n>m
  --userns=keep-id
  --timeout=<seconds>
  --mount type=bind,src=<workspace>,dst=/workspace,ro
  --workdir=/workspace
  --                              # ends option parsing (verify, §9)
  <image@sha256:…>
  <command argv…>
```

Rules the argv builder enforces and unit-tests as a pure function:
- image is `name@sha256:<64 hex>` and cannot start with `-`; the command is
  always after the image;
- prefer `--mount type=bind,…` over `--volume=src:dst`; reject `,`, `:` and
  non-UTF-8 in mount paths so a path can never add options;
- never forward `spec.env` wholesale; every env key is allow-listed;
- `--pull=never`: the image must already be local. Pulling is `container.pull`,
  a separate, Network-class, policy-checked step;
- the plan reports **requested** flags; the **result** carries the engine's
  `inspect` read-back (cap set, `NoNewPrivileges`, read-only rootfs, network
  mode, memory limit). An enforcement state is `Enforced` only when the
  read-back confirms it (decision DEC-031 in 67-containers).

Docker equivalents use the same flags (`--read-only`, `--cap-drop`,
`--security-opt no-new-privileges`, `--pids-limit`, `--network none`); the
`HostConfig` fields `ReadonlyRootfs`, `CapDrop`, `SecurityOpt`, `PidsLimit`,
`NetworkMode` exist in Engine API 1.54.

### 5.4 Approval and audit

Reuse the tunnel contract (`docs/design/tunnel-policy-v1.md`): a canonical
approval string over (engine, image digest, profile, mounts, network, command
hash); approval on first use and on any change; reconnect reuses only an
unchanged approval. Audit goes through the same `harw::audit` target as
`shell.host_escalation`.

### 5.5 Output

Bounded stdout/stderr (bytes and lines), exit code, wall time, read-back
report, container ID. Errors are `Ok(ToolOutput::Error)` with a one-line
cause plus a next step ("image not local: call `container.pull`"), never a
panic and never raw engine stderr dumped unbounded.

## 6. What this means for PR #74

The review comment on #74 lists the defects; here is how each maps to this
design:

| #74 behaviour | Needed |
|---|---|
| `spec.program` placed where the image belongs | explicit image (from the catalog) then command; `--` before the image |
| `no_new_privs` / `capabilities` reported `Enforced`, no flag passed | pass `--security-opt=no-new-privileges`, `--cap-drop=all`; report from read-back |
| `SandboxPolicy.filesystem` ignored; workspace always rw | derive mounts and `ro` from the profile |
| `--volume=src:dst` string concatenation | `--mount` with rejected separators |
| `env_clear()` + fixed `PATH` in remote mode | env allowlist incl. `HOME`, `SSH_AUTH_SOCK` (remote only) |
| no module-level `cfg(target_os = "linux")` | gate like `harw-job-executor-bwrap` |

## 7. Discoverability: the model should reach for it first

1. **`tool.index`** (PR #89 / `TOOL-DISCOVERABILITY.md`): the `container`
   family appears with a one-line "use when" ("run a command in an isolated
   container; call `container.engine` first"). A lint fails a tool without it.
2. **Shell guard**: when `shell.exec` is asked to run `docker`/`podman`/`buildah`/
   `skopeo`, return a hint from the existing denial-hint path pointing at
   `container.run`, **instead of** pointing at host-mode escalation.
3. **Visible defaults**: `[tools.container]` appears in the generated defaults
   TOML (engine, profiles, image catalog, limits) so a person can see and
   change them.
4. **`container.engine` as the preflight**: returns `available`, `rootless`,
   `profiles`, `images`, and for every disabled capability a reason.
5. **Credential coverage**: add Podman's `containers/auth.json` and
   `~/.config/containers` to the credential-path classifier next to `.docker`
   (`auto_classifier.rs:179`), and add `docker.sock`/`podman.sock` to the path
   denylist (the OpenClaw note in `docs/planning/75-harness-patterns/openclaw.md:89`
   lists "Docker-socket aliases"; our code has no equivalent today).

## 8. Other software we use constantly

### 8.1 The shared pattern

Every row below should follow the template `harw-cli/src/pr_review.rs`
(`gh`) and `harw-tool-tunnel` (ssh) already use:

1. typed arguments → `Command` with one `.arg()` per value; **no** `/bin/sh`;
2. a **pure policy core** (validation, canonical approval string, argv
   builder, redaction) testable without the binary;
3. a **fixed binary path** and scrubbed environment;
4. **modes** instead of free flags (`Inspect` / `Build` / `Fetch`, as in
   `CargoExecutionMode`); network only in the mode that needs it, via the
   egress policy;
5. job-owned lifecycle for anything long-running (`harw-tool-job`), bounded retry;
6. bounded, redacted, structured output;
7. permission checked before argument parsing; one `CapabilityClass` per tool;
8. a `tool.index` "use when" line and a `[tools.<family>]` defaults section.

### 8.2 Candidates (ordered by how often the repo itself uses them)

| Software | Evidence | Proposed family | Modes / risk | Notes |
|---|---|---|---|---|
| **cargo** (check, clippy, fmt, nextest, doc, deny) and `cargo xtask gates` | CONTRIBUTING gate list; `Makefile`; `CargoSandboxProfile` | `cargo.check`, `cargo.test`, `cargo.clippy`, `cargo.fmt_check`, `cargo.gates` | `Inspect` / `BuildOffline` run in the cargo bwrap profile (Shell class); `Fetch` needs network grant | Highest value: the gate list is fixed, results are parseable (`--message-format=json`). Verify exact JSON flags per subcommand **(unverified)** |
| **git** | `Command::new("git")` ×2; every PR flow | `git.status`, `git.diff`, `git.log`, `git.show` (read); `git.commit` approval | read = Read class; no `push`, no `reset --hard`, no `config`, no hooks | Pass `-c core.hooksPath=/dev/null` and `--no-pager` **(unverified flags)**; refuse paths outside the workspace |
| **gh** | `harw-cli/src/pr_review.rs` | `gh.pr_view`, `gh.pr_diff` (read) | Network class; posting is a separate approval, as `--post` is today | Promote the existing runner logic instead of rewriting it |
| **systemd user units / Quadlet / journalctl** | `deploy/systemd/`, `packaging/podman/quadlet/`, 3 × `systemctl` in `harw-install` | `service.status`, `service.logs`, `service.restart` | Only units named `harw-*`; Host class; `restart` needs approval | Quadlet files regenerate on `daemon-reload`; the tool must validate with the generator dry-run before reload (per Podman docs). `podman auto-update --dry-run` as read-only check |
| **ssh / tailscale / cloudflared** | `harw-tool-tunnel` (scaffold), #88 policy core, `harw-tailscale` crate | `tunnel.*` (planned) | Already specified in `tunnel-policy-v1/v2` | Finish `tunnel.*` before adding more; do not add a second ingress story next to Pingora (see `docs/maintainers/pingora-review-gate.md`) |
| **make / cross** | `Makefile`, `Cross.toml`, `release.yml` | `cargo.cross_build` | Host class (needs a container engine) | Must set `CROSS_CONTAINER_ENGINE` explicitly; cross defaults to Docker when both exist |
| **bwrap** | `harw-job-executor-bwrap` | none (infrastructure, not a tool) | — | Keep it behind the executor |

### 8.3 What not to wrap

- `sudo`/`doas`/`pkexec`: already refused even in host mode; keep it that way.
- Registry login, `docker context create`, anything that writes credentials.
- Generic "run any CLI with a flag map": it recreates `shell.exec` with extra steps.

## 9. Verification log (what I did and did not check)

Verified:
- Docker/Podman/`cross` facts in §2–§3 against the upstream pages in §10.
- Docker Engine API 1.54 spec (downloaded): multiplexed stream framing,
  `ErrorResponse {message}`, `HostConfig` fields, `/events` container events.
- Repo statements by reading the cited files; PR #74's code read in full.
- Local environment: Docker client 29.6.2 / API 1.55 is installed; **no daemon
  is running**, and Podman, skopeo, buildah and `cross` are **not installed**.

Not verified (do these in the first implementation PR, as integration tests
that skip when the engine is absent):
1. That `--` before the image is accepted by `podman run` and by `docker run`.
2. `--format json` output shape per subcommand (`ps`, `images`, `inspect`,
   `info`) and the field that says "rootless" for each engine.
3. The default capability set of rootless Podman and of Docker on our
   target distros; this is why the plan always passes `--cap-drop=all`.
4. Behaviour of `--mount` when a path contains `,` (we reject it regardless).
5. `cargo` JSON message flags and `git` hardening flags listed in §8.2.
6. Docker Content Trust and BuildKit/buildx behaviour (not researched).
7. The Engine API version supported by the Docker on our worker nodes (docs
   say 1.56 is latest; the local client speaks 1.55).

## 10. Open decisions

- **D1:** add `Permission::ManageContainers`, or reuse `ExecuteProcess`
  plus the Host class? (Recommendation: new permission, §4.3.)
- **D2:** CLI argv builder (A) for tools and REST (B) only for the executor?
  (Recommendation: yes, §4.1.)
- **D3:** is Docker supported at all, or Podman only? The plans say "Podman,
  Docker"; Docker adds a rootless-detection path and a second set of
  read-back parsers. (Recommendation: Podman first; Docker behind the same
  trait later.)
- **D4:** where do image digests live: config catalog (recommended) or a
  lock file next to `Cargo.lock`?
- **D5:** order of delivery. Proposed: (1) `container.engine` + argv builder
  + tests (no engine needed), (2) `container.run` hermetic, (3) `cargo.*`,
  (4) `container.pull` + policy.json, (5) `service.*`, (6) `git.*`/`gh.*`.
- **D6:** do `cargo.*` tools run inside bwrap or in a container? Bwrap is
  already there and cheaper; containers matter when the toolchain must be
  pinned or a different libc/arch is needed.

## 11. Sources

Upstream:
- Docker Engine API overview: https://docs.docker.com/reference/api/engine/
- Docker Engine API v1.54 spec: https://docs.docker.com/reference/api/engine/version/v1.54.yaml
- Protect the Docker daemon socket: https://docs.docker.com/engine/security/protect-access/
- Docker rootless mode: https://docs.docker.com/engine/security/rootless/
- `podman run`: https://docs.podman.io/en/latest/markdown/podman-run.1.html
- `podman system service`: https://docs.podman.io/en/latest/markdown/podman-system-service.1.html
- `podman --remote`: https://docs.podman.io/en/latest/markdown/podman-remote.1.html
- Rootless shortcomings: https://github.com/containers/podman/blob/main/rootless.md
- Quadlet: https://docs.podman.io/en/latest/markdown/podman-systemd.unit.5.html
- `podman auto-update`: https://docs.podman.io/en/latest/markdown/podman-auto-update.1.html
- `podman compose`: https://docs.podman.io/en/latest/markdown/podman-compose.1.html
- `podman login` (auth file): https://docs.podman.io/en/latest/markdown/podman-login.1.html
- `containers-policy.json`: https://github.com/containers/image/blob/main/docs/containers-policy.json.5.md
- `cross` README (engines): https://github.com/cross-rs/cross

In this repo: `docs/planning/67-containers/`, `docs/planning/87-tool-gaps/` and
`TOOL-DISCOVERABILITY.md` (PR #88), `docs/design/tunnel-policy-v1.md`,
`packaging/podman/`, `harw-job-executor-bwrap/src/executor.rs`,
`harw-tool-shell/src/host_escalation.rs`, `harw-sandbox/src/cargo.rs`,
`harw-cli/src/pr_review.rs`, `harw-registry-defaults/src/capability_catalog.rs`,
`harw-authority/src/lib.rs`.

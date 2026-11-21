# Harw-DoD (Detect · Orient · Defend)

> Status: partially implemented · Last reviewed: 2026-11-08

Harw-DoD (short: DoD) is Harwness's on-device observation-and-defense suite: it watches
what is happening on the host around agent activity (process starts,
network connections, file writes, authentication, resource pressure, …)
and — behind an explicit, disabled-by-default enforcement path — can act on
what it observes. It is a **separate, privileged, opt-in install**: a
set of crates under `harw-dod/` (members of the root Cargo workspace since
PL-60, built with `-p` selections — one `Cargo.lock`, no nested workspace)
with its own Makefile, its own system accounts, and its own FHS layout. Nothing in `make install`/`make service`
(the ordinary Harwness install described in
[`docs/setup/install.md`](install.md)) touches DoD, and nothing in DoD
requires it.

This page explains what DoD is made of, how it is installed, and — as
honestly as the code allows — what currently works and what is still a
contract rather than a live capability.

**Naming.** "Harw-DoD" is the name of the suite in human-facing text. Crate
names (`harw-dod`, `harw-dod-*`), binary names (`harw-sentinel`,
`harw-probe-*`, `harw-warden`), FHS paths (`/etc/harw-dod`,
`/var/lib/harw-dod`, …), socket paths, groups, config keys and unit file
names keep their existing spelling; renaming them would break every
installation for no functional gain (see the
[facade charter](../design/harw-dod-charter.md#2-the-name)).

## Operator flow (PL-97)

The short path from a fresh host to `harw dod status` reporting a running,
harw-aware suite. Each step links to the details further down.

1. **Install the suite** (once, as the unprivileged operator, not as
   root): `make dod-install` ([Installation](#installation)) builds as the
   invoking user and then escalates by itself (`sudo -E`) for the file
   install and the enable step; `build` refuses to run as root, so
   `sudo make dod-install` fails. Alternatively `make dod-build`, then
   `sudo make -C harw-dod install-existing`, which installs the already built
   artifacts and skips the enable step. `install` ends
   by running `scripts/service.sh enable`, which validates
   `/etc/harw-dod/config.toml` first; on a fresh host without a chosen
   `active_profile` that last step refuses and nothing is started (the
   files, accounts and units are installed by then).
2. **Give harw a delegated cgroup root** so its jobs land in
   `harw-job-<attempt>` cgroups: run harw under `harw service install`
   (`harw-serve` user unit) or `deploy/systemd/harw-control.service`, or
   start it with `systemd-run --user --scope -p Delegate=yes harw …`, or
   set `[jobs] cgroup_root` ([Observe harw jobs](#observe-harw-jobs-pl-97-e3)).
3. **Generate the profile** as the harw user: `harw dod config suggest`
   prints the `[profiles.harw-jobs]` stanza for the resolved root plus the
   matching `[dod] allowed_cgroup_prefixes` for harw's own config. The
   stanza is filled in only when `[jobs] cgroup_root` is set or
   `HARW_CGROUP_ROOT=<root>` is exported; otherwise `config suggest` prints
   the `<R>` template and lists the service-unit cgroups
   (`system.slice/harw-control.service`, `…/app.slice/harw-serve.service`)
   as candidates. `harw dod doctor` falls back to a delegated service-unit
   cgroup, `config suggest` does not;
   `harw dod config suggest --drop-in` prints the sentinel and probe
   drop-ins. Merge the profile into `/etc/harw-dod/config.toml` (root-owned,
   not group/world-writable) and check it with
   `sudo harw dod config check`.
4. **Enable**: `sudo harw dod enable` (the same steps as
   `make -C harw-dod enable`: config check, `systemctl daemon-reload`,
   `systemctl enable --now harw-dod.target`). The Warden stays disabled.
5. **Read the posture**: add the operator to `harw-dod-readers`
   ([Exports read by harw](#exports-read-by-harw)), log in again, then
   `harw dod status`, `harw dod doctor` (prerequisites, the three
   cgroup-root checks and the execution chain table), `harw dod findings
   -n 5` and `harw dod sensors`. In the TUI: `/dod status` and `/dod view`;
   models get the read-only tools `dod.status`, `dod.findings` and
   `dod.sensors` and, on first use, the bundled `harw-dod` skill.
   `harw doctor` adds one `chain/<stage>` check per readiness stage
   (`installed`, `configured`, `running`, `ready`), warnings only.

**Running as root (euid 0).** `harw dod enable|disable|restart` and
`harw dod warden enable|disable` require euid 0 and otherwise exit `3`
("refused") without changing anything; `enable` and `restart` also exit
`3` without a valid `active_profile`. The read-only actions run as any
user. Under `sudo`, the harw home is the one of root's environment unless
you pass `--home`, so `[jobs] cgroup_root` of the operator's config is not
seen; run `config suggest` and `doctor` as the harw user. As euid 0 harw
never treats its own cgroup as a delegated root (resolver step 3), and
`harw dod doctor` reports the cgroups of the harw service units (looked up
for `SUDO_UID`) only as candidates with the reason "root can write every
cgroup".

**Known gaps.** `/dod config suggest` in the chat prints the static
template with `<R>` placeholders (only the CLI fills in the resolved root);
`harw serve` and `harw web` do not read `[jobs] cgroup_root` or
`[jobs] trampoline`; jobs ↔ findings correlation (a per-job `⛨` badge,
`job_id` filter) is not built yet (PL-97 E4); a TUI mark for the execution
chain report does not exist (the report is in `harw doctor` and
`harw dod doctor`).

## Purpose

An agent harness that can execute arbitrary commands needs a way to answer
"what did this actually do to the machine, and can I stop it". DoD is that
answer, split into two halves:

- **Observation** — a set of *sensors*, each reading exactly one host
  source (a `/proc` file, a sysfs directory, cgroup v2 accounting, netlink
  audit records, or a kernel event via eBPF) through exactly one
  capability, and turning it into a typed reading or security event.
- **Escalation and enforcement** — turning a sensor's findings into rule
  evaluations, and, if a signed authorization says so, into an enforcement
  action (freeze a cgroup, cut network egress, kill a process tree). This
  half is shipped but **disabled** in every default install; see
  [Warden](#the-warden-shipped-but-disabled).

## Components

The DoD domain (`harw-dod/crates/`, part of the root workspace) has 34 crates.
Grouped by role, with what each one is responsible for (the list below
names the main ones):

**Access and data vocabulary** (shared foundations, not sensors themselves)
- `harw-dod-cap` — the capability/read-scope vocabulary every sensor is
  built against (`Capability`, `ReadScope`, `SensorHandle`, …).
- `harw-dod-signals` — the reading/event vocabulary (`Sensor` trait,
  `SensorReading`, `SecurityEvent`, `HostSample`, …).
- `harw-dod-readfs` — the single filesystem seam unprivileged sensors read
  through; no sensor opens a path itself.
- `harw-dod-fixtures` — the shared fixture/test harness all sensor crates
  inherit from.

**Sensors** (each owns exactly one host source and one capability)
- `harw-dod-cpu` (`/proc/stat`), `harw-dod-memory` (`/proc/meminfo`),
  `harw-dod-thermal` (`/sys/class/thermal`), `harw-dod-gpu`
  (`/sys/class/drm`), `harw-dod-blockio` (block device stats in sysfs),
  `harw-dod-netcounters` (`/proc/net/dev`), `harw-dod-cgroup` (cgroup v2
  accounting), `harw-dod-listener` (open listening sockets with cgroup
  attribution), `harw-dod-workspace` (structural drift in this repo's own
  Cargo dependency graph), `harw-dod-scanreport` (reads third-party
  scanner reports; never invokes a scanner itself), `harw-dod-authlog`
  (login events via `auid`, backed by `harw-dod-netlink`),
  `harw-dod-fsmon` (fanotify-based file-write watcher, library half of
  `harw-probe-fs`), `harw-dod-procmon` (process start/exit via eBPF,
  library half of `harw-probe-bpf`).

**eBPF and translation infrastructure**
- `harw-dod-bpf` — the eBPF loading layer behind a trait, shared by
  `-procmon` and `-flow`.
- `harw-dod-flow` — translates raw eBPF connection events into
  `SecurityEvent`s (TCP connect observation).
- `harw-dod-netlink` — the netlink transport `-authlog` reads audit
  records through.

**Evaluation**
- `harw-dod-rules` — turns observations into `Finding`s: rules, baselines,
  severity.
- `harw-dod-sentinel` — the unprivileged collection point: holds sensors,
  polls them, tracks health, buffers what they produce. Contains no
  source-parsing logic itself.

**Escalation and enforcement**
- `harw-dod-escalate` — the one place a `Finding` becomes an authorized
  action.
- `harw-dod-warden-proto` — the wire protocol between escalation and the
  enforcer.
- `harw-dod-warden` — the enforcer (library half): freeze/release cgroups,
  cut network egress, kill process trees.
- `harw-dod-netpolicy` — a pure data type describing which egress rules
  should apply for a scope; applies nothing itself.

**Facade and running processes**
- `harw-dod` — the curated facade intended for the rest of the workspace;
  a consumer that wants "observe the host" or "evaluate a finding" should
  depend on this, not on individual sensor crates (no workspace crate
  depends on it yet).
- `harw-sentinel` — the unprivileged binary: runs the sensors and the
  collection loop.
- `harw-probe-fs` — the privileged (`CAP_SYS_ADMIN`) fanotify probe binary;
  push-only, no receive path. Part of `harw-dod.target`; it watches the
  persistence locations from `[probe_fs]` (see
  [below](#fanotify-probe-watch-scope-pl-100-wp16)).
- `harw-probe-bpf` — the privileged (`CAP_BPF`, `CAP_PERFMON`) eBPF probe
  binary; push-only, no receive path.
- `harw-warden` — the enforcer binary, listening on a systemd-activated
  `SOCK_SEQPACKET` socket.

## Data flow

```
   unprivileged                      privileged (per-probe capability)
 ┌───────────────┐   /proc,/sys,    ┌───────────────┐
 │  harw-sentinel │◄── cgroup v2 ────│                │
 │ (poll loop,    │   netlink        │                │
 │  no source     │                  │                │
 │  parsing)      │◄── sentinel.sock ┤ harw-probe-fs  │  fanotify (CAP_SYS_ADMIN)
 │                │                  │ harw-probe-bpf │  eBPF (CAP_BPF, CAP_PERFMON)
 └───────┬────────┘                  └───────────────┘
         │ readings / SecurityEvents
         ▼
 ┌───────────────┐
 │ harw-dod-rules │  findings, baselines, severity
 └───────┬────────┘
         │ Finding
         ▼
 ┌────────────────┐   signed authorization required
 │ harw-dod-escalate│─────────────────────────────┐
 └────────────────┘                               │
                                                    ▼
                                          ┌───────────────┐
                                          │  harw-warden   │  freeze/release cgroup,
                                          │ (own user,     │  cut egress, kill tree
                                          │  socket-       │
                                          │  activated,    │
                                          │  DISABLED by   │
                                          │  default)      │
                                          └───────────────┘
```

`harw-sentinel` is the single unprivileged collection point. The two
privileged probes (`harw-probe-fs`, `harw-probe-bpf`) are **push-only** —
by design, a process holding `CAP_SYS_ADMIN` or `CAP_BPF`/`CAP_PERFMON`
never *accepts* connections, it only sends to the sentinel's socket. Each
probe holds exactly the one capability class its job requires and nothing
more.

## Privilege model

- **System accounts** (`deploy/sysusers.d/harw.conf`, installed via
  `systemd-sysusers`, no login shell, no home directory), one per binary:
  `harw-sentinel`, `harw-probe-fs`, `harw-probe-bpf`, `harw-warden`. The
  Warden never runs as `root`.
- **Groups:** `harw-dod-config` gives the sentinel and the eBPF probe
  read-only access to `/etc/harw-dod`; `harw-ipc` lets the sentinel create,
  and the two probes connect to, `/run/harw/sentinel.sock`;
  `harw-warden-clients` (empty by default) may connect to
  `/run/harw/warden.sock` — add the escalation client explicitly;
  `harw-dod-readers` (empty by default) may read the sentinel exports in
  `/var/lib/harw-dod/export` — see
  [Exports read by harw](#exports-read-by-harw).
- **Capabilities, per binary, and nothing beyond them:**
  `CapabilityBoundingSet`/`AmbientCapabilities` are empty for
  `harw-sentinel` (no elevated capability at all); `CAP_BPF CAP_PERFMON`
  for `harw-probe-bpf` (the loader refuses to attach tracepoint/FEntry
  programs without both); `CAP_SYS_ADMIN` for `harw-probe-fs` (the only
  capability that permits `fanotify_mark` with `FAN_MARK_FILESYSTEM` and
  the `FAN_UNLIMITED_MARKS` flag it needs); `CAP_DAC_OVERRIDE CAP_NET_ADMIN`
  for `harw-warden` (writing the root-owned `cgroup.freeze`/`cgroup.kill`
  files, and running `nft` for network isolation).
- **Root-owned files and FHS directories** (default prefix `/usr/local`;
  `PREFIX=/usr` for a distribution package):
  - programs: `/usr/local/libexec/harw-dod`
  - eBPF objects + manifest: `/usr/local/lib/harw-dod/bpf`
  - configuration: `/etc/harw-dod`
  - state/telemetry: `/var/lib/harw-dod`, `/var/log/harw-dod`
  - sentinel exports: `/var/lib/harw-dod/export` (`findings.jsonl`,
    `health.json`)
  - runtime sockets: `/run/harw/sentinel.sock`, `/run/harw/warden.sock`
    (`/run/harw` is created by `deploy/tmpfiles.d/harw.conf`; the
    infrastructure daemons use `/run/harw/infra/`)
- Each systemd unit additionally sets `NoNewPrivileges=yes`,
  `ProtectSystem=strict`, `ProtectHome=yes`, `PrivateTmp=yes`,
  `SystemCallFilter=@system-service` (plus the few syscalls the binary
  needs, e.g. `bpf`, `perf_event_open`, `fanotify_*`, Landlock), and
  scopes its writable paths to exactly what that process needs
  (`ReadWritePaths`).
- The package **never** installs into a user's `~/.harw` tree, and never
  runs as, or touches files owned by, a regular user account.

## How it integrates with harw

The main Harwness config carries a `[dod]` section
(`harw-config/src/dod_toml.rs`) that holds advisory escalation settings for
display and the doctor (not binding policy) — separate from DoD's own `/etc/harw-dod/config.toml`,
which is the system-level sensor/profile configuration DoD itself reads.

> **The escalation keys of `[dod]` are advisory.** `kill_requires_human`,
> `auto_freeze`, `proof_key_dir` and `allowed_cgroup_prefixes` authorize,
> enable or disable no action. The binding TCB policy lives only in
> root-owned files under `/etc/harw-dod/` (see the end of this section).
> Today only `allowed_cgroup_prefixes` is read outside config loading (by
> the doctor, for its cgroup-root checks); `kill_requires_human` and
> `auto_freeze` are validated or merged at load time and used for nothing
> else (the doctor does not show `auto_freeze` today). `export_dir` is
> different: harw's read-only posture and display path consumes it
> (`harw-runtime/src/assembly.rs`, via the security-signals context
> provider) to locate the sentinel exports. `findings_window_hours` is
> validated at load time; no consumer outside config loading was found in
> the code today, so it is configuration for that same display path.

Components that do not exist yet are marked **planned (PL-100 Phase N)**
(German: "geplant (PL-100 Phase N)"); the plan is in
[`docs/planning/100-dod-ai-integration/`](../planning/100-dod-ai-integration/README.md).

The `[dod]` section's defaults are deliberately restrictive
("secure/off"):

- `kill_requires_human` — must be `true`; rejected in every configuration
  layer if set to `false`. Advisory: it documents that a kill is never
  taken without a human, but it is not what enforces that.
- `auto_freeze` — **advisory only; no action depends on it** (default
  `true`). It does not control automatic freezes: nothing freezes
  automatically. A repository layer can only keep it at `true`, never
  turn it off.
- `proof_key_dir` — where the keys for `SignedAuthorization` verification
  live; never inherited from an untrusted config layer. Advisory; the
  Warden/escalator use their own root-owned policy.
- `allowed_cgroup_prefixes` — which cgroup path prefixes the escalation
  chain may address at all; an untrusted layer may only narrow this list,
  never widen it. Advisory; only the doctor reads it, the Warden/escalator
  use their own root-owned policy. The one canonical form is an absolute path strictly
  below the cgroup2 mount (`/sys/fs/cgroup/<…>`, trailing `/` optional);
  a relative entry is rejected. `harw_config::dod_toml::warden_cgroup_id`
  converts an entry to the Warden's relative `CgroupId`
  (`/sys/fs/cgroup/user.slice/harw.slice` → `user.slice/harw.slice`).
- `export_dir` — explicit directory of the sentinel exports that harw
  reads (absolute path). Unset (default): `/var/lib/harw-dod/export` if
  readable, else `<HARW home>/dod/export`. Like `proof_key_dir`, never
  taken from an untrusted (repository) layer — a repository must not be
  able to point harw at fabricated posture.
- `findings_window_hours` — how far back harw summarizes exported findings
  (default `24`, must be at least `1`); set by trusted layers only.

These invariants are checked while the `[dod]` table is loaded: a `[dod]`
table in any layer — trusted or not — with `kill_requires_human = false`, a
blank or non-canonical (relative, outside `/sys/fs/cgroup`)
`allowed_cgroup_prefixes` entry, a relative `export_dir` or
`findings_window_hours = 0` makes harw's config loading fail with a
`[dod]: …` error instead of starting with that value.

Enforcement itself lives entirely in the escalation/warden binaries, and
their binding policy is **not** this section. It comes from root-owned
files that harw cannot change:

- `/etc/harw-dod/config.toml` — sensor/profile configuration, loaded with
  `load_system_config` (requires root-trusted ownership of the path).
- `/etc/harw-dod/warden.toml` — the Warden's own proof policy and deny
  list; **planned (PL-100 Phase 3.1)**, it does not exist yet and the
  Warden reads no policy file today.

### Planned keys (PL-100)

None of the following tables exists in the code yet. The `[dod]` table is
parsed with `deny_unknown_fields`, so **adding `[dod.triage]`,
`[dod.approve]` or `[dod.watch]` to a config today makes config loading
fail**. Do not add them until the matching phase has shipped.

`[dod.triage]` — **planned (PL-100 Phase 1.9; the triage run itself Phase
2)**. The triage model is the owner's explicit choice:

- `model` (`"<provider>/<model>"`), `effort`, `max_tokens`, `max_parallel`.
- Set via the `/dod model set|show|clear` command or the TUI settings
  (PL-100 Phase 1.9), or by editing the trusted config file.
- Read from trusted layers only (home/profile). A repository layer can
  neither set nor change the model.
- Without a `model`, no automatic triage runs; the watch mode falls back
  to `notify` with a clear notice. There is **no silent fallback to the
  session model**.
- `harw dod doctor` will show provider, model and locality (loopback or
  Unix socket is local, anything else is remote). The locality is shown,
  not enforced; with a remote model the provider sees bounded evidence
  cards.

`[dod.watch]` — **planned (PL-100 Phase 2.2)**:

- `mode = "off" | "notify" | "triage"` (default `triage`). `notify` shows
  deterministic notices only; `triage` additionally runs the triage model
  (needs `[dod.triage].model`).
- `autoconnect` — the `harw-dod-watch` systemd user unit connects to the
  gateway on start. Default: on (README O5: the unit connects
  automatically); the exact default and the behaviour of
  `autoconnect = false` are not yet fixed (planned, PL-100 Phase 2.2). The
  unit is planned too.
- `[dod.dispatch] owner = true` — only one dispatcher per host (host
  lock); **planned (PL-100 Phase 2.2)**.

`[dod.approve]` — **planned (PL-100 Phase 3.3)**:

- `tui_tty` (default `true`; written to `~/.harw/config.toml` by `install`
  and `update`) allows an approval via `pkttyagent` in the TUI's own
  terminal. A compromised harw can alter what that terminal displays, so
  the binding to the shown proposal is weaker; the mode therefore covers
  only the reversible actions freeze and thaw. Kill and isolation are
  refused in this mode. The doctor and Mission Control label it as a
  weaker binding.

### Approval of interventions (planned, PL-100 Phase 3)

No approval path exists today. Once built, a freeze, thaw, kill or
isolation takes effect only after a human approval that a compromised harw
cannot forge, bound to the escalator's proposal digest (action, target
cgroup, kernel ID, finding ID, rule, severity, hold). Two paths:

1. **Independent (path A, full guarantee).** The operator starts
   `harw-dod-approve` themselves in a session harw does not control: their
   own terminal window, another login, or SSH. It fetches proposals from
   the escalator, shows typed and escaped fields (action, cgroup kid,
   path, rule, severity, short hash, digest), asks the operator to type the
   short hash and optionally a hold duration, and then polkit
   `auth_admin` (no keep) follows. It refuses peers inside a harw agent
   session or a `harw-job-*` scope. **Planned (PL-100 Phase 3.3).**
2. **Started from the TUI (path B).** The TUI only sends
   `RequestApproval{proposal_id}` to the escalator; harw starts no
   `pkexec` and no dialog. The escalator calls polkit
   `CheckAuthorization` with details taken from its own proposal (action,
   cgroup, rule, short hash, digest) and the desktop's graphical polkit
   agent renders them. A different `proposal_id` shows a different
   dialog, so harw cannot misrepresent what is approved. This path has no
   free-text input, so the hold is "until lifted"; set a hold duration via
   path A. **Planned (PL-100 Phase 3.3).**

**Kill and network isolation** are not part of the ordinary freeze
approval: they need the separate polkit action
`org.harw.dod.irreversible` and an explicit "irreversible" confirmation
(`irreversible_ack`), and are never possible in the same-TTY mode.
**Planned (PL-100 Phase 3.1 and 3.3).**

Read-only companions: `harw dod proposals` and `harw dod frozen` (active
freezes and isolations; **planned, PL-100 Phase 3.3**), `harw dod verdicts`
(**planned, PL-100 Phase 2.4**).

### Planned components at a glance

| Component | State |
|---|---|
| `[dod.triage]`, `/dod model` | planned (PL-100 Phase 1.9) |
| `DodWatch`, dispatch, `[dod.watch]`, `harw-dod-watch` user unit | planned (PL-100 Phase 2.1-2.2) |
| Verdict store, `harw dod verdicts` | planned (PL-100 Phase 2.3-2.4) |
| `harw-escalator` (today's crate: `harw-dod-escalate`), proposals | planned (PL-100 Phase 3.2) |
| `warden.toml`, Warden deny list, kill/isolation with `irreversible_ack` | planned (PL-100 Phase 3.1) |
| `harw-dod-approve`, `RequestApproval`, polkit actions, `[dod.approve]` | planned (PL-100 Phase 3.3) |
| `harw dod proposals`, `harw dod frozen` | planned (PL-100 Phase 3.3) |
| `security-hunter`, `/dod tune` | planned (PL-100 Phase 4) |
| `harw dod warden enable/disable/status`, `/dod status`, `/dod view` | implemented |

### Observe harw jobs (PL-97 E3)

harw runs every job in its own `harw-job-<attempt>` cgroup below one
**delegated cgroup v2 root** `R`. The same `R` drives harw's executors,
the DoD profile and the Warden prefixes. harw resolves it in this order
(`harw_job_runtime::host::resolve_delegated_root`):

1. `[jobs] cgroup_root` in harw's home or profile config (an untrusted
   repository layer cannot set it). It applies to the interactive and
   one-shot `harw` paths (chat, TUI) and to the verify runner; `harw serve`
   and `harw web` do not read it yet and use the next steps;
2. the `HARW_CGROUP_ROOT` environment variable (the verify runner still
   accepts the deprecated `HARW_VERIFY_CGROUP_ROOT`);
3. the cgroup harw itself runs in (its parent when harw sits in the
   `harw-main` leaf, see below), when there is real delegation evidence:
   writable cgroup2 plus systemd's `user.delegate`/`trusted.delegate`
   xattr, or, outside a user manager, a cgroup systemd handed to the
   unit's `User=`. Write access alone is not enough (every cgroup below
   `user@<uid>.service` belongs to the user, so a terminal's `app-*.scope`
   would pass), and this step never applies as euid 0. The `harw-serve`
   user unit (`harw service install`) and
   `deploy/systemd/harw-control.service` both set `Delegate=yes`.
   `harw dod config suggest` and `harw dod doctor` skip this step: the CLI's
   own cgroup (a terminal scope or the sudo session) is never harw's job
   root. Without steps 1–2 they list the service roots as candidates;
4. none: jobs run without their own cgroup, and harw logs the hint to
   start it under `systemd-run --user --scope -p Delegate=yes harw …` or
   from one of those units.

`R` must be an absolute path strictly below `/sys/fs/cgroup`; anything
else is refused. Both units also set `DelegateSubgroup=harw-main`: on
systemd 254 or later harw itself runs in the leaf `R/harw-main`, `R` holds
no process, and the memory/pids controllers can be enabled for the job
cgroups. Older systemd ignores the key with a warning. harw then lives in
`R` itself, so the kernel's "no internal processes" rule can keep the
memory/pids controllers off for the job cgroups: membership, kill and
observation work, and the limits may only be partial. The same holds for
the `systemd-run --user --scope -p Delegate=yes` route (scopes have no
`DelegateSubgroup=`); to get limits there, point `[jobs] cgroup_root` or
`HARW_CGROUP_ROOT` at a child cgroup of the scope that holds no process.

On installs where `harw-job-exec` (the job sandbox trampoline) does not sit
next to `harw`, set `[jobs] trampoline = "/abs/path/harw-job-exec"` (trusted
layers only; same paths as `[jobs] cgroup_root`, `harw serve` and
`harw web` look next to `harw`). `harw doctor` warns when the trampoline is
missing.

`harw dod config suggest` prints, for the resolved `R`, the DoD profile
and the matching harw entry:

```toml
[profiles.harw-jobs]
scope = "cgroups"
cgroup_paths = ["/user.slice/user-1000.slice/user@1000.service/app.slice/harw-serve.service"]
include_descendants = true
sensors = ["exec", "tcp-connect"]

# harw's own config:
[dod]
allowed_cgroup_prefixes = ["/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/harw-serve.service"]
```

The profile's `cgroup_paths` are relative to the probe's `--cgroup-root`
(the cgroup2 mount, `/sys/fs/cgroup`) with a leading `/`; harw's
`allowed_cgroup_prefixes` use the canonical absolute form. Values are
printed as TOML strings with escaping, so systemd escapes in cgroup names
(`\x2d`) survive. Without a resolved root the command prints the template
with `<R>` placeholders, the service roots that exist on this host, and
the probe ordering requirement below.

`harw dod config suggest --drop-in` prints two systemd drop-ins:

- `harw-sentinel.service.d/harw-jobs.conf` resets `ExecStart=` and adds
  `--cgroup-root <R>`, so the sentinel's cgroup sensor reports the
  `harw-job-<attempt>` cgroups (the direct children of `R`).
- `harw-probe-bpf.service.d/harw-jobs.conf` handles ordering. **`R` must
  exist before `harw-probe-bpf` starts**: with a `scope = "cgroups"`
  profile the probe refuses to start while no configured cgroup root
  resolves. When `R` belongs to a system unit (for example
  `/system.slice/harw-control.service`) the drop-in adds
  `BindsTo=<unit>`, `PartOf=<unit>` and `After=<unit>`. The probe resolves
  cgroup ids once at start, and every start of the unit creates `R` anew:
  `BindsTo=` stops the probe when the unit stops or fails (no silent watch
  on a stale cgroup), `PartOf=` restarts it with the unit. After a crash
  and automatic restart (`Restart=on-failure`) run `sudo harw dod restart`.
  When `R` belongs to the user manager or to a transient scope, systemd
  cannot order the probe after it. Start harw first
  (`systemctl --user start harw-serve`), then run `sudo harw dod restart`,
  and run it again after **every** `harw-serve` restart (`Restart=always`,
  `harw update`): each restart gives `R` a new cgroup id. New job cgroups
  below `R` are picked up by the probe's rescan (every 2 s) without a
  restart.
- The sentinel `ExecStart=` argument doubles `\` and `%`, so systemd
  passes `R` verbatim (no unescaping, no specifier expansion).

`harw dod doctor` adds three checks, all warnings only: `cgroup-root`
(`R` resolves), `profile-root` (the active profile has a `cgroup_paths`
entry at or above `R` with `include_descendants = true`, or observes the
host) and `allowed-prefixes` (every `allowed_cgroup_prefixes` entry lies
within `R`).

**Out of scope:** harw does not pass `allowed_cgroup_prefixes` to the
Warden's `ProofPolicy`. That wiring belongs to the v2 proof path
(PL-96 W01); the Warden still verifies v1 proofs. Starting services stays
an explicit operator action: installing harw starts nothing.

### Exports read by harw

The sentinel writes two files for harw to read. `harw-security-hub` can
also read `findings.jsonl` (its `[findings] path`), but only after its
service has been granted the reader group — on a stock install it cannot
(see [Letting `harw-security-hub` read the findings](#letting-harw-security-hub-read-the-findings)).
Both are optional sentinel flags; the shipped unit enables both:

| File | Flag | Written | Content |
|---|---|---|---|
| `/var/lib/harw-dod/export/findings.jsonl` | `--findings-export <abs path>` | appended per **reported** finding (repeats inside the dedup cooldown are not written); rotated at 16 MiB (`--findings-export-keep`) | one JSON object per line: `finding_id` (UUIDv4), `host`, `rule_id`, `severity`, `summary` (escaped, ≤ 512 bytes), `observed_at` (time of the triggering observation), `schema_version: 3`, an optional `scope` (cgroup path) plus `scope_obfuscated`, and the v3 fields `instance` (UUIDv4 per sentinel start), `seq` (from 1; a gap means a lost line), `kind`, `hardness`, `dedup_key`, `occurrences`, `first_seen`, `cgroup_kid?`, `record_digest?` (BLAKE3 of the spooled record) and `evidence_card?` (≤ 4 KiB: event counts, agreeing `uid`/`destination`/`path`/`process{pid,comm}`, all escaped) |
| `/var/lib/harw-dod/export/health.json` | `--health-export <abs path>` (default: `health.json` next to the findings export) | replaced atomically (temporary file + rename) every poll cycle | `schema_version: 1`, `written_at`, `config_digest`, `profile` (both `null` on built-in defaults), `sensors[]` (`id`, `health` = `bound`/`retrying`/`degraded`, `degrade_reason?`, `last_ok_at?`, `readings_total`, `errors_total`), `ipc` (`probes_connected`, `events_total`, `dropped_total`, `peers_rejected_total`, `events_last_cycle`, `dropped_last_cycle`, `events_last_60s`, `dropped_last_60s`), `ring` (ring entries evicted before the rules saw them: `overflow_{samples,events}_{total,last_cycle,last_60s}`), `windows` (`last_cycle_ms`, `last_60s_ms`), `spool` (`null` without a spool; else `state` = `ok`/`degraded`, `degrade_reason`, cumulative and last-60-s counters), `landlock` (`outcome` = `fully_enforced`/`partially_enforced`/`not_enforced`/`failed`, `requested_abi`, `effective_abi` — the kernel's level, `null` without Landlock), `listener_history` (`state` = `ok`/`degraded`, `degrade_reason?`, `keys`, `primed`) |

Versions 2 and 3 of the findings line are additive: a line without
`schema_version` is version 1, and every reader ignores unknown fields, so
old and new lines parse alike (roll out readers before the sentinel). The
`health.json` additions keep `schema_version: 1`. Neither file contains raw
events, configuration contents or secrets; the evidence card carries only
escaped, bounded summaries.

**Spool and dedup state** (not exports, owner-only, PL-100 WP0.4/WP0.12).
With `--findings-export` the sentinel freezes one `FindingRecord` per
reported finding (triggering events plus up to 32 ring events as context,
≤ 64 KiB) into `<--home>/spool` (`/var/lib/harw-dod/spool`, `0700`,
written by a bounded writer thread). The spool keeps 14 days and at most
1 GiB, including records nobody consumed. `<--home>/finding-throttle.json`
(`0600`) persists the dedup cooldown per `dedup_key` across restarts.
`<--home>/listener-history.json` (`0600`, PL-100 WP1.6) persists the
listeners the `listener-new` rule already knows: on the very first start the
sentinel learns every open listener silently and reports only listeners that
appear later. A history that cannot be used (unreadable, larger than 1 MiB,
corrupt, foreign version) never stops the sentinel, but it is never relearned
silently either: the first cycle exports a `sensor-degraded` finding for
`sentinel-listener-history`, `health.json` reports `listener_history` as
`degraded` (with `state_unreadable`, `state_not_regular_file`,
`state_too_large` or `state_invalid`) until the next restart, and the journal
names the reason. Do not create these files by hand or through tmpfiles.

**Startup checks of the sentinel (PL-100 owner review).**

- *Destinations.* Before any writer opens, the sentinel compares every
  file it writes by resolved identity (canonical directory plus file name):
  the findings export and its rotations `<name>.N`, the health document
  and its temporary sibling `.<name>.tmp`, the throttle state
  `<--home>/finding-throttle.json` with its `.finding-throttle.json.tmp`,
  the listener history `<--home>/listener-history.json` with its
  `.listener-history.json.tmp`, and the IPC socket (`--socket`, default
  `<--home>/sentinel.sock`), which is unlinked and re-bound at every start.
  `--socket` must be absolute; a socket in a directory that does not exist
  yet is not compared (binding there fails anyway). Independently, the bind
  only ever removes an existing *socket*: any other entry at the socket path
  (a state file, a directory) is left alone and the IPC receive path stays
  off for that run.
  If two of them alias each other (also through a symlinked directory), or
  a directory cannot be resolved, the sentinel refuses to start and exits
  with "refusing to start: export and state destinations are unsafe". Keep
  the exports out of `--home` itself; the shipped layout
  (`--home /var/lib/harw-dod`, exports in `/var/lib/harw-dod/export`)
  passes.
- *IPC after Landlock.* The sentinel binds `sentinel.sock` before its
  Landlock self-restriction but starts the accept and receive threads only
  afterwards, so every thread that parses probe messages runs inside the
  sandbox. A probe that connects in between waits in the socket backlog.
- *Undecodable probe messages.* A message from an admitted probe that is
  not a valid event or exceeds the receive buffer is discarded and counted:
  once per poll cycle the sentinel logs one `warn` line with the count
  (never the payload) and reports the total as the meta-metric
  `sensor_parse_errors_total` (expected: `0`).

**Non-Linux builds.** The sentinel's IPC path and Landlock are Linux-only;
on other Unix targets the binary still compiles and runs without them
(no probe socket, Landlock reported as not enforced). Check that build
with

```sh
rustup target add x86_64-unknown-freebsd
cargo check -p harw-sentinel --target x86_64-unknown-freebsd
```

Build scripts of dependencies with C or assembly parts may need a C
compiler for the target (`CC_x86_64_unknown_freebsd`). Non-Unix targets
(Windows) are out of scope: workspace crates the sentinel depends on
(`harw-fsutil`, `harw-home`, the spool in `harw-dod-sentinel`) are
Unix-only.

### Sentinel flags added by PL-100 Phase 0

| Flag | Default | Meaning |
|---|---|---|
| `--finding-cooldown-secs <n>` | `[sentinel] finding_cooldown_secs`, else `600` (0 … 604800) | a repeat of the same `dedup_key` (rule, uid, cgroup, coarse subject such as /24 or /64 plus port) inside this window only raises `occurrences`; stronger hardness, higher severity or a clock jump back report it again |
| `--probe-peer-user <name\|uid>` | `[sentinel] probe_peer_users`, else `harw-probe-bpf`, `harw-probe-fs` | accounts whose processes may submit events over the IPC socket (`SO_PEERCRED`; repeatable, replaces the configured list and the default). uid 0 is always allowed; other peers are closed unread and counted in `ipc.peers_rejected_total` |
| `--workspace-root <abs dir>` | none | the workspace drift sensor and its Landlock read root run only with an explicit absolute directory; `/` and relative roots are refused |
| `--container-sensor <auto\|on\|off>` | `[sentinel] container_sensor`, else `auto` | container sensor (PL-100 WP1.8): `auto` and `on` run one sensor per known layout (`libpod-*.scope` below `/sys/fs/cgroup/machine.slice`, `docker-*.scope` below `system.slice`), even when no runtime is detected at start (rootful Podman is daemonless, and the sentinel starts before the first container); `off` runs none. With `auto` on a cgroup v2 root, a cgroup directory that does not exist yet (systemd creates `machine.slice` only with the first container) is an empty reading, never a degraded sensor; without a cgroup v2 root the container sensors report themselves degraded at the first poll. With `on`, a layout whose runtime was detected neither by its socket (`/run/podman/podman.sock`, `/run/docker.sock`) nor by a running scope at start and that has no cgroup directory reports the sensor as degraded. Docker with the `cgroupfs` driver (`/sys/fs/cgroup/docker/<id>`, no `.scope` names) is not supported: with `auto` and `on` the sentinel reports it once per start as `sensor-degraded` (sensor `container-docker-cgroupfs`) instead of staying silently blind. Every container scope is reported only as `container_unowned_<id>` (the sentinel has no list of containers harw started) |
| `--print-sandbox` | off | prints the effective Landlock read/read-write roots, refused roots with their reason, the state files below `--home` (`state-file`), where the probe accounts come from (`ipc-peer-source`: `command-line`, `dod-config` or `built-in`), the admitted IPC peer uids and the container switch with its source, layouts and their availability rule (`container-sensor`, `container-layout` … `on-demand`/`required`) and unsupported layouts (`container-unsupported`), then exits (run it with the same flags as the unit) |

**Sentinel settings in `config.toml` (PL-100).** The optional table
`[sentinel]` of `/etc/harw-dod/config.toml` sets the two values above
without editing the unit; a command-line flag always wins, and without flag
and key the built-in default applies:

```toml
[sentinel]
probe_peer_users = ["harw-probe-bpf", "harw-probe-fs"]   # at most 32; [] admits uid 0 only
finding_cooldown_secs = 600                              # 0 ... 604800
container_sensor = "auto"                                # "auto", "on" or "off"
```

The table is additive (`schema_version` stays `1`), but readers built before
it refuse a file that contains it — update every DoD component (sentinel,
probes, harw) before adding the table. The shipped files show it commented
out.

**Sysfs alias roots (PL-100 WP1.10).** The entries of `/sys/class/thermal`,
`/sys/block` and `/sys/class/drm` are symlinks into `/sys/devices`. The
thermal, blockio and GPU sensors therefore read them through alias roots
(one symlink level per class entry into `/sys/devices`), and the sentinel's
Landlock ruleset grants `/sys/devices` read-only; `--print-sandbox` lists it
as `landlock\tread\t"/sys/devices"`. Fixture roots outside `/sys` stay plain
roots. `/proc` was already a read root; the auid (`/proc/<pid>/loginuid`) is
read by `harw-probe-fs`, whose Landlock ruleset now includes `/proc`
read-only.

#### fanotify probe watch scope (PL-100 WP1.6)

`harw-probe-fs` is part of `harw-dod.target`. Its unit passes
`--config /etc/harw-dod/config.toml` (group `harw-dod-config`; a missing,
untrusted or invalid file refuses the start) and no placeholder root. The
watch scope comes from the optional table `[probe_fs]`, per key, else from
built-in defaults that cover the locations of the `persistence-write` rule:

```toml
[probe_fs]
# every write below these directories counts
scope_roots = ["/etc", "/var/spool/cron", "/var/spool/at", "/usr/lib/systemd",
               "/usr/local/lib/systemd", "/lib/systemd", "/usr/share/systemd",
               "/usr/local/share/systemd", "/usr/lib/udev/rules.d",
               "/usr/local/lib/udev/rules.d", "/lib/udev/rules.d"]
# directories holding home directories, or home directories themselves
home_roots = ["/home", "/var/home", "/root", "/var/roothome"]
# below a home root only paths that start, after at most two components
# (the user name, or domain and user name), with one of these entries count
home_entries = [".ssh", ".config/systemd", ".local/share/systemd", ".config/autostart",
                ".config/environment.d", ".pam_environment", ".bashrc", ".bash_aliases",
                ".bash_profile", ".bash_login", ".bash_logout", ".profile", ".zshrc",
                ".zprofile", ".zshenv", ".zlogin"]
```

No user name appears in the file, and a directory an attacker creates later
(`~/.config/autostart`) is still covered: Landlock and the fanotify marks
span the whole home roots, only forwarding is filtered (a browser cache
write never reaches the sentinel). Roots are absolute, never `/`, at most 64
per list. `--scope-root`/`--home-root` replace both root lists,
`--home-entry` the entries; the shipped files show the table commented out
(reader-first). The unit uses `ProtectHome=read-only` instead of `yes`, so
the home roots stay visible to the probe. The probe also:

- coalesces repeated writes of one process to one file within 2 s into one
  event (`FAN_MODIFY` and `FAN_CLOSE_WRITE` give the same `file-write`);
- keeps the kernel bound on its event queue (`fs.fanotify.max_queued_events`,
  default 16384; no `FAN_UNLIMITED_QUEUE`), so a writer faster than the probe
  cannot grow kernel memory without bound, and reports a queue overflow as
  `SensorDegraded` of its sensor id (`fsmon-0`), which the sentinel exports
  as a `sensor-degraded` finding;
- places its fanotify marks before the Landlock self-restriction: a root the
  probe account cannot read (`/var/spool/cron` and `/var/spool/at` with mode
  `0700`, `/root`) is marked through its nearest readable ancestor on the
  same filesystem (`FAN_MARK_FILESYSTEM` marks the whole filesystem anyway).
  If a root lies on a filesystem without any readable path at or above it
  (for example a separate `/var` or `/root` with mode `0700`), its writes are
  not observed: the probe logs a `warn` line with counts only and sends one
  `SensorDegraded` of its sensor id at start, so the sentinel exports a
  `sensor-degraded` finding instead of staying silently blind;
- marks every filesystem mounted below a root at start as well (read from
  `/proc/self/mountinfo`, at most 4 MiB; `autofs` triggers are not touched):
  an NFS or autofs home, a systemd-homed or ecryptfs mount below `/home` is
  marked through its mount point, and one it cannot mark counts as uncovered
  and leads to the same start-time `SensorDegraded`. Mounts that appear after
  the start (systemd-homed or autofs at login) and roots that were absent at
  start (for example `/var/spool/cron` before cronie is installed; their
  number is logged on `info`) stay unobserved until the probe restarts
  (`sudo systemctl restart harw-probe-fs.service`). Like a root itself, a
  hung hard NFS mount below a root stalls the start;
- opens every event descriptor and every mark target non-blocking
  (`O_NONBLOCK`), so a FIFO without a writer below a watched root cannot stall
  the probe;
- reads `/proc/<pid>/loginuid` for the auid (Landlock read root `/proc`);
- treats an event the kernel could not open for it (a write on the marked
  filesystem outside the Landlock read roots, `EACCES`/`EPERM`) as skipped,
  not as a sensor error.

**Rule configuration of the host (PL-100 WP1.6).** Before its Landlock
self-restriction the sentinel builds the configuration of the path and
listener rules: the canonical `--workspace-root` (starts below it are an
`exec-writable-path` anomaly), every distinct home directory from
`/etc/passwd` that is not already a fixed location (`/home/<name>`, `/root`,
`/var/home/<name>`, `/var/roothome`) or a system directory (`/`, `/usr/…`,
`/bin`, … are skipped; at most 256), and the ephemeral port range from
`/proc/sys/net/ipv4/ip_local_port_range` (default `32768 60999`). Home
directories are never canonicalised (that would walk every path component
and could trigger an automount or hang on a stale NFS server before the
first poll); only a symlink directly below `/` (for example
`/home -> var/home`) is read with `readlink` and the home is added in that
resolved spelling as well. The journal shows only counts (`rule
configuration resolved`), never a home path. On `warn`, a finding carries only its rule id, severity and
`occurrences`; summaries (which may contain escaped paths with account
names), ids, the dedup key and the scope are on `debug` only.

The egress rule uses the active profile's `egress_allow_cidrs` as its
allowed network scope; with an empty list every egress flow is a High
finding (a loopback destination is Low). `harw dod doctor` therefore
reports `egress-allowlist` as missing while `[dod.watch] mode = "triage"`
is in effect and the list is empty.

**Permissions.** `deploy/tmpfiles.d/harw.conf` creates the export directory
as `2750 harw-sentinel:harw-dod-readers` (setgid, so the files inherit the
group) and the state directory as `0710 harw-sentinel:harw-dod-readers`
(members may traverse to `export/`, not list the state directory). The
files themselves are `0640`. The group `harw-dod-readers` is **empty by
default**; to let a user's harw read the posture:

```sh
sudo usermod -aG harw-dod-readers <user>   # takes effect at the user's next login
```

(`harw dod` has no `--grant-read` option; group membership stays a manual
step.)

#### Letting `harw-security-hub` read the findings

`harw-security-hub.service` runs as `harw-security-hub:harw-security` and
is **not** a member of `harw-dod-readers`; the shipped unit grants no
supplementary group, so the group stays memberless by default. Until you
grant it, the hub cannot even traverse `/var/lib/harw-dod`: reading its
export fails with `EACCES`, and `GET /v1/posture` reports its DoD part as
`"status": "unavailable"` with no findings.

The hub has no built-in findings source: `[findings]` is off unless
`/etc/harw-security-hub/config.toml` names it, `path` is required, and
validation refuses `[findings]` without `[server] host`, the correlation key
that must equal the `host` field the sentinel writes (its `--findings-host`,
or the kernel hostname from `/proc/sys/kernel/hostname` when that flag is
not set). Add both before the drop-in, then check the file:

```toml
# /etc/harw-security-hub/config.toml (excerpt)
[server]
host = "host-a"   # same value as the sentinel's `host` field

[findings]
path = "/var/lib/harw-dod/export/findings.jsonl"
```

```sh
sudo harw-security-hub --config /etc/harw-security-hub/config.toml --check
```

To grant read access to the hub service only (not to the hub account at
large), add a drop-in and restart the hub:

```sh
sudo mkdir -p /etc/systemd/system/harw-security-hub.service.d
printf '[Service]\nSupplementaryGroups=harw-dod-readers\n' \
  | sudo tee /etc/systemd/system/harw-security-hub.service.d/dod-readers.conf
sudo systemctl daemon-reload
sudo systemctl restart harw-security-hub.service
```

The group must exist (it is created by `systemd-sysusers` from
`deploy/sysusers.d/harw.conf` during the DoD install); with the drop-in in
place and the group missing, the hub service fails to start. Remove the
drop-in before deleting the group (see [Uninstalling](#uninstalling)).

**Where harw looks.** The shared resolver (`harw-dod-client`, PL-97 D2)
picks the export directory in this order: `[dod] export_dir`, then
`/var/lib/harw-dod/export` if it is a directory this process can list,
then `<HARW home>/dod/export` only when the system path is absent (missing,
a symlink or not a directory). A
system export directory that exists but cannot be listed is reported as
unreadable (with the hint to join `harw-dod-readers`), never replaced by
the home fallback. Only the system export counts as verified; findings from
`export_dir` or the home fallback are labelled `UNVERIFIED`. The CLI, the
`/dod` command, the `dod.*` model tools, the TUI and the opt-in
`[memory] security_signals` context source all use this resolver.

## Installation

DoD is built and installed from `harw-dod/`, or via the root `Makefile`'s
delegation targets (`make dod-build`, `make dod-install`, `make
dod-enable`, `make dod-uninstall` — thin wrappers around `make -C harw-dod
<target>`; run `make -C harw-dod help` or `harw-dod/README.md` for the complete
target list).

1. **Prerequisites.** `make -C harw-dod doctor` gives a read-only report of what
   is present. Building the eBPF artifacts needs `clang`, `bpftool`, and
   `llvm-objdump` at versions pinned in `harw-dod/bpf/toolchain.lock.toml`
   (checked fail-closed by `harw-dod/bpf/scripts/verify-toolchain.sh`), plus a
   kernel exposing BTF (`/sys/kernel/btf/vmlinux`). `make -C harw-dod
   ensure-bpf-toolchain` locates these tools (searching `PATH`, then
   `/usr/sbin`, `/sbin`) and, only when one is genuinely missing and
   `apt-get`/root privileges are available, installs it — this is the
   first step of both `build` and `install`.

2. **Build.**
   ```sh
   make dod-build         # from the repo root, or: make -C harw-dod build
   ```
   Builds the release host binaries (`harw-sentinel`, `harw-probe-bpf`,
   `harw-probe-fs`, `harw-warden`) and the pinned eBPF artifacts
   (`bpf/exec.bpf.o`, `bpf/exit.bpf.o`, `bpf/tcp_v4_connect.bpf.o`,
   `bpf/tcp_v6_connect.bpf.o`, `bpf/manifest.json` — build outputs, never
   checked in). Must run unprivileged; it refuses to run as root.

3. **Install.**
   ```sh
   make dod-install   # unprivileged, from the repo root, or: make -C harw-dod install
   ```
   Run it as the unprivileged operator, not with `sudo`: `install` depends
   on `build`, which refuses to run as root, and escalates by itself
   (`sudo -E`, one prompt) for the file install and the enable step.
   Rebuilds (to avoid installing stale artifacts), then installs files and
   creates the system accounts (`systemd-sysusers`), runtime
   directories (`systemd-tmpfiles`), and systemd units — all taken from
   the repository's `deploy/` tree (see
   [below](#systemd-units-and-the-deploy-tree)) — and reloads the
   systemd daemon. Unit files of the old layout (`harw-dod-sentinel.service`,
   `harw-dod-bpf.service`, `harw-dod-warden.*`, `harw-dod.conf`) are
   removed. As its last step `install` runs the enable action of step 5
   (`scripts/service.sh enable`), which validates the config first: without
   a chosen `active_profile` it refuses, so on a fresh host nothing is
   started and `install` reports that failure after the files are in
   place. `sudo make -C harw-dod install-existing` installs already built
   artifacts without the enable step. `DESTDIR=/path/to/stage make -C harw-dod
   stage` runs the same `install` target into a staging root; it, too,
   ends with the enable step (flagged in PL-97 §8.4 against the "no silent
   services" policy, still open).

4. **Configure.** Copy the shipped example and choose exactly one profile:
   ```sh
   sudo cp /etc/harw-dod/config.toml.example /etc/harw-dod/config.toml
   sudo editor /etc/harw-dod/config.toml
   ```
   Add exactly one `active_profile = "…"` under `[profiles.<name>]`.
   `scope = "host"` (observe the whole machine) is never selected
   implicitly — you must write it explicitly if that's what you want.
   Shipped example profiles (`harw-dod/packaging/config.example.toml`):
   - `selected-services` — `scope = "cgroups"` over named systemd service
     cgroups.
   - `user-workloads` — `scope = "cgroups"` over `/user.slice`.
   - `host` — `scope = "host"`, the entire machine.
   All three default to `sensors = ["exec", "tcp-connect"]` and an empty
   egress allowlist. `make -C harw-dod install` preserves an existing
   `/etc/harw-dod/config.toml`; only `config.toml.example` is
   installed/replaced.

5. **Enable.**
   ```sh
   sudo harw dod enable    # or: make dod-enable / make -C harw-dod enable (as root)
   ```
   Runs the explicit configuration/profile validation first and refuses to
   enable without a chosen `active_profile` (`harw dod enable` exits `3`). This starts and enables
   `harw-sentinel.service`, `harw-probe-bpf.service` and
   `harw-probe-fs.service` under `harw-dod.target` — observation only,
   never the Warden. The fanotify probe watches the persistence locations
   from `[probe_fs]` or its built-in defaults (see
   [above](#fanotify-probe-watch-scope-pl-100-wp16)); a drop-in with
   `--scope-root`/`--home-root` narrows or widens that.

### The Warden — shipped but disabled

`harw-warden.service` and its `SOCK_SEQPACKET` activation socket are
installed by `make dod-install` but have **no `[Install]` section**, no
dependency from `harw-dod.target`, and an additional
`ConditionPathExists=/etc/harw-dod/warden.enable` gate. Normal install,
enable, restart, and observation-target operations never start or enable
either unit. To opt into enforcement explicitly:

```sh
sudo harw dod warden enable   # or: sudo make -C harw-dod enable-warden
```

This creates `/etc/harw-dod/warden.enable`, reloads systemd, and enables
the socket. Harw's own `[dod]` config (see
[above](#how-it-integrates-with-harw)) authorizes nothing: it is advisory.
The authorization path (independent `harw-dod-approve` or polkit via the
escalator, see [above](#approval-of-interventions-planned-pl-100-phase-3))
is **planned (PL-100 Phase 3.3)** and does not exist yet; the Warden's own
policy file is **planned (PL-100 Phase 3.1)**.

## Current status and limitations (honest)

- **PL-100 (AI triage, approval) is planned, not implemented.** The
  `[dod.triage]`, `[dod.watch]` and `[dod.approve]` keys, the escalator
  submission and the approval paths described above do not exist yet.
- **The observation half (sentinel + probes) is what's live.** The
  escalation/warden chain is implemented as a library and a socket-
  activated binary, but is disabled by every default install path, as
  described above.
- **Binary integration boundary.** Both observation units pass the
  configuration explicitly: `harw-sentinel --config
  /etc/harw-dod/config.toml --home /var/lib/harw-dod --socket
  /run/harw/sentinel.sock --findings-export …/export/findings.jsonl
  --health-export …/export/health.json`, `harw-probe-bpf --config …` and
  `harw-probe-fs --config …` (the fanotify probe, too, refuses to start on
  an unusable explicit configuration).
  With an explicit `--config`, a missing, untrusted (wrong owner,
  group/world writable, symlink) or invalid configuration makes the
  sentinel refuse to start; only when started **without** `--config` (by
  hand) does it fall back to built-in defaults if
  `/etc/harw-dod/config.toml` does not exist, with a warning. The planned
  separate `--state-dir --telemetry-dir --runtime-dir` flags do not exist;
  `--home` is the state directory. Verify current behavior against
  `harw-dod/crates/harw-sentinel/src/{cli,main}.rs` and
  `harw-dod/crates/harw-probe-bpf/src/main.rs` before relying on a specific CLI
  surface.
- **eBPF artifacts are never checked into the repository**; they must be
  built locally (or by your packaging pipeline) with the pinned toolchain
  before `install` will proceed — `install` refuses to run until they
  exist.
- **`stage-test`** (`make -C harw-dod stage-test`) checks the packaging
  contract statically, without building or installing anything — useful
  for CI that can't run eBPF builds.

## systemd units and the deploy tree

`deploy/` at the repository root is the **single** source of every Harwness
system unit (Crypto Masterplan v2 §22, H10); there is no second copy under
`harw-dod/`:

| Path | Content |
|---|---|
| `deploy/systemd/harw-dod.target`, `harw-sentinel.service`, `harw-probe-bpf.service`, `harw-probe-fs.service`, `harw-warden.service`, `harw-warden.socket` | DoD (installed by `make -C harw-dod install`) |
| `deploy/systemd/harw-infra.target`, `harw-control.{socket,service}`, `harw-auth-hub.{socket,service}`, `harw-netsec.{socket,service}`, `harw-security-hub.{socket,service}` | infrastructure daemons (not installed by the DoD package) |
| `deploy/sysusers.d/harw.conf` | all system accounts and groups |
| `deploy/tmpfiles.d/harw.conf` | `/run/harw`, `/run/harw/infra`, DoD state/log directories, the sentinel export directory |

The same files are compiled into `harw` byte-identically (`harw-install`
parity tests fail on any drift). To inspect exactly what is shipped, with
the default install paths substituted:

```sh
harw install --print-systemd                      # every unit
harw install --print-systemd harw-warden.socket   # one unit
```

`harw-dod/scripts/install.sh` substitutes the `@LIBEXECDIR@`, `@BPFDIR@`,
`@SYSCONFDIR@`, `@STATEDIR@` and `@LOGDIR@` placeholders from its own
variables and refuses to install a unit with an unresolved placeholder.
Runtime sockets live at fixed paths: `/run/harw/` for DoD,
`/run/harw/infra/{control,secure,network,security}.sock` for the
infrastructure daemons. Every infrastructure daemon is socket-activated
(`--systemd-socket`): `harw web` behind `harw-control.{socket,service}`,
`harw-auth-hub`, `harw-netsec` and `harw-security-hub`. Only systemd
creates sockets in `/run/harw/infra`, so the directory is `0755 root:root`;
access is set per socket unit (mode `0660` plus a client group such as
`harw-control-clients`).

## Uninstalling

```sh
sudo make dod-uninstall   # from the repo root, or: sudo make -C harw-dod uninstall
```

Removes only the files listed in `harw-dod/packaging/manifest` (binaries, eBPF
objects, systemd units, sysusers/tmpfiles config snippets, and the
`config.toml.example`). It deliberately leaves `/etc/harw-dod/config.toml`,
state, logs, runtime data, and the system accounts in place, so an upgrade or a rollback never silently discards
operator evidence or configuration. Remove those by hand if you want a
full teardown:

```sh
sudo systemctl disable --now harw-dod.target harw-sentinel.service harw-probe-bpf.service harw-probe-fs.service
# only if you added it (see "Letting harw-security-hub read the findings"):
sudo rm -f /etc/systemd/system/harw-security-hub.service.d/dod-readers.conf && sudo systemctl daemon-reload
sudo rm -rf /etc/harw-dod /var/lib/harw-dod /var/log/harw-dod
sudo rm -f /run/harw/sentinel.sock /run/harw/warden.sock
for u in harw-sentinel harw-probe-fs harw-probe-bpf harw-warden; do sudo userdel "$u"; done
sudo groupdel harw-dod-config; sudo groupdel harw-ipc; sudo groupdel harw-warden-clients; sudo groupdel harw-dod-readers
```

## Troubleshooting

- `make -C harw-dod doctor` — read-only report of what prerequisites are
  present/missing.
- `make -C harw-dod check-config` — validates `/etc/harw-dod/config.toml`
  (or `CONFIG=<path>`) and prints the selected profile without touching
  any service.
- `make -C harw-dod status` / `make -C harw-dod logs` — inspect the observation
  services (`CONFIG=<path>` to check a non-default config file first).
- `install refuses to run` — most commonly means the eBPF artifacts
  haven't been built yet (`make -C harw-dod build-bpf`) or a pinned toolchain
  tool is missing/wrong-version (`make -C harw-dod doctor`,
  `bpf/scripts/verify-toolchain.sh`).
- `enable refuses to run` — no `active_profile` selected in
  `/etc/harw-dod/config.toml`; see [Configure](#installation) above.
- `make -C harw-dod smoke` — an explicit, operator-triggered live smoke check
  (`SMOKE_CONFIRM=1`); it is never part of ordinary install/enable/tests.

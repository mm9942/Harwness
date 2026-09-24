# Build prerequisites

This document lists what must be installed and correctly configured on a
machine before `cargo build`/`cargo test`/`xtask` can even start in this
workspace — tool versions and, for the Raspberry Pi 5 as a target platform,
kernel properties that no `Cargo.toml` entry can enforce. It does not
replace an installation guide (see `harw-install`/
`docs/design/CONTRACT-setup-install.md` for that) — it records **which**
prerequisites apply and **why**, so a deviation is visible instead of
silent.

## 1. Rust toolchain: `1.98.1` (pinned)

The toolchain is pinned in **`rust-toolchain.toml`** at the repository
root (`channel = "1.98.1"`, components `rustfmt` and `clippy`, profile
`minimal`). rustup reads this file automatically — including in the
standalone `dod/` workspace, which therefore has no file of its own — and
installs the version on the first `cargo` invocation. CI
(`.github/workflows/ci.yml`, `release.yml`) installs exactly this file via
`rustup toolchain install`; locally and in CI the same compiler, the same
`rustfmt` and the same `clippy` are used.

Separately, the workspace root `Cargo.toml` sets `rust-version = "1.85"` as
the MSRV floor — the *lowest* version this workspace must compile against
per the manifest, not a statement about the toolchain that is actually
tested.

```
$ rustup show active-toolchain
1.98.1-x86_64-unknown-linux-gnu (overridden by '…/rust-toolchain.toml')
```

## 2. `crypt_guard` `3.0.1` (crates.io, hybrid KEM)

`harw-secrets` depends on `crypt_guard` at the **crates.io** version
`3.0.1` — not on a path dependency pointing at a sibling repository. This
version does not cover plain ML-KEM for the deterministic seed→KEK
derivation used here, so `harw-secrets` instead uses a hybrid-KEM
construction. The full rationale, the affected modules
(`harw-secrets/src/{policy.rs,kek.rs,envelope.rs}`) and the migration
decision are in **`docs/setup/crypt-guard.md`** — this document only
points there instead of duplicating it.

## 3. Bubblewrap (`bwrap`)

`harw-sandbox`/`harw-tool-shell` isolate child processes via Bubblewrap
(see `harw-sandbox/src/bwrap.rs`); `harw-install`'s doctor check
`sandbox/bwrap` (`harw-install/src/doctor.rs:126-146`) looks for the
binary in `PATH`. On Debian-based systems (Debian, Raspberry Pi OS), the
`bubblewrap` package installs it at the fixed path `/usr/bin/bwrap`:

```
$ apt install bubblewrap
$ /usr/bin/bwrap --version
```

If `bwrap` is missing, the doctor check reports "Bubblewrap isolation not
available" instead of a hard failure — but sandbox isolation is then
genuinely inactive, not just a warning without consequence.

## 4. `prlimit` (from `util-linux`)

Resource limits for child processes are set/read via `prlimit`, not via a
reimplemented `setrlimit` library. `prlimit` is part of the `util-linux`
package, which is practically standard on every Linux distribution
(including Raspberry Pi OS); it is listed as an explicit prerequisite here
anyway because a minimal container or chroot image can omit it.

```
$ prlimit --version
```

## 5. `git` ≥ `2.40`

Needed for tooling that relies on more modern Git features (including
`git worktree` usage and partial/sparse checkouts elsewhere in the
project). Older `git` versions (in particular the 2.3x releases
preinstalled on some long-term-support distributions) do not support some
of these flags, or behave differently.

```
$ git --version
git version 2.40.0 (or newer)
```

## 6. Raspberry Pi 5 (aarch64) — divergent kernel behavior

The Raspberry Pi 5 is a target platform for this project, but its default
kernel diverges from a typical x86_64 server kernel in three
security-relevant ways. All three affect the privileged warden/sentinel
binaries, not a normal `cargo build`:

- **No Landlock.** The kernel shipped with Raspberry Pi OS does not
  include Landlock support by default. This is handled **asymmetrically**:
  a hard startup failure for the three privileged binaries, degradation
  with `SensorDegraded` for the sentinel. A build on a Pi 5 with the
  default kernel must expect exactly this behavior, not a silent
  fallback.
- **No BTF** (BPF Type Format) in the default kernel image. Any
  BPF-backed observation (`harw-probe-bpf`, `harw-dod-bpf`) that relies on
  `CO-RE` (Compile Once – Run Everywhere) via BTF needs either a
  custom-built kernel with `CONFIG_DEBUG_INFO_BTF=y`, or must fall back to
  the respective degradation strategy instead of the BTF-backed path.
- **16K page size.** Current Raspberry Pi OS kernels offer (sometimes by
  default, sometimes as an option) a page size of 16 KiB instead of the
  4 KiB common on most Linux systems. Code that assumes a page size
  instead of querying it via `sysconf(_SC_PAGESIZE)`/the Rust equivalent,
  or that computes mapped memory regions in 4 KiB steps, produces wrong
  results on this platform without the build itself failing.
- **`cgroup_disable=memory`.** Some Raspberry Pi OS images set this boot
  argument by default (originally to spare the Pi's limited RAM the cost
  of memory-cgroup accounting). Any component that reads memory limits or
  counters via the memory cgroup (`harw-dod-cgroup`, `harw-dod-memory`)
  sees no data on such a machine — which must be distinguished from an
  actual read error. Anyone who wants memory-cgroup accounting active on
  the Pi 5 must explicitly add `cgroup_enable=memory cgroup_memory=1` to
  `/boot/firmware/cmdline.txt` and reboot.

None of these four points prevents a `cargo build` on the Pi 5 itself —
they change the **runtime behavior** of the privileged binaries and the
DoD sensors compared to a typical development machine.

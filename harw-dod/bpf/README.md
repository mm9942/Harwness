# DoD BPF v1 build domain

This directory is intentionally independent of Cargo.  Its four C sources
produce one program per ELF: `sched_process_exec`, `sched_process_exit`, and
task-context `tcp_v4_connect`/`tcp_v6_connect` fentry hooks.  All share the
byte-addressed header in `include/harw_dod_wire_v1.h`.

`make` first requires exact `clang`, `bpftool`, and `llvm-objdump` output
lines in `toolchain.lock.toml`. The checked-in lock records the Ubuntu
toolchain validated on the target host. A different distribution or tool
upgrade can change any of these lines; a mismatch reports the expected and
actual versions and stops the build. Use the locked tools, or record the
installed versions in a candidate lock and validate them with
`make -C harw-dod/bpf TOOLCHAIN_LOCK=/path/to/candidate.lock.toml OUT=/tmp/dod-bpf-check`
before updating the committed lock. The build:

- derives `vmlinux.h` from the target BTF;
- builds `bpfel` objects with deterministic source paths;
- checks each expected program symbol, ELF section, and map name; and
- writes a manifest binding object SHA-256 values, target BTF SHA-256, ABI,
  program/attach names, architecture, and locked tool versions.

Despite its `.json` name, `manifest.json` is TOML (schema 1). `install.sh`
installs it root-owned with mode 0644 next to the objects, and
`harw-probe-bpf --bpf-manifest <BPFDIR>/manifest.json` verifies every object
against its `sha256` entry at runtime, before Landlock and before any load
(`harw-dod/crates/harw-probe-bpf/src/bpf_manifest.rs`). The manifest and every
directory above it must be root-owned and neither group- nor world-writable,
so a manifest inside a developer checkout is rejected; install first. A
missing flag, an untrusted or malformed manifest, a missing entry or a digest
mismatch makes the probe report `SensorDegraded` and exit with status 78
(`EX_CONFIG`); the systemd unit sets `RestartPreventExitStatus=78`, so the
refusal is not retried.

`scripts/write-manifest.sh` writes every value on one line and checks that
shape before it replaces the old manifest.
`make -C harw-dod/bpf check-manifest-writer` needs no BPF toolchain: it re-runs
the writer on fixed dummy inputs and diffs the result against
`harw-dod/crates/harw-probe-bpf/src/testdata/manifest.sample.toml`, the golden
file the probe's parser test reads. After an intended schema change,
update the parser first, then run `scripts/check-manifest-writer.sh --update`.

**Rollout:** manifests written before PL-100 Phase 4 are not valid TOML (the
BTF digest line contained `sha256sum`'s newline inside the string), so the new
probe refuses to start with them. On every existing host, rebuild and
reinstall the BPF artifacts (`make -C harw-dod build-bpf`, then
`harw-dod/scripts/install.sh`, e.g. via `make -C harw-dod install`) before or
together with the new `harw-probe-bpf` binary and unit. `make` rebuilds the
manifest by itself because it depends on `scripts/write-manifest.sh`.

The loader must use the manifest-verified bytes exactly once to construct a
`BpfObjectContract`; it rejects substituted programs/maps and populates the
profile cgroup map before attaching a hook.  A profile with descendants must
already contain every resolved descendant cgroup ID at attach time.  A new or
recreated cgroup is not silently added; it requires the configured restart.

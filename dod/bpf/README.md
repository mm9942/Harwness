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
`make -C dod/bpf TOOLCHAIN_LOCK=/path/to/candidate.lock.toml OUT=/tmp/dod-bpf-check`
before updating the committed lock. The build:

- derives `vmlinux.h` from the target BTF;
- builds `bpfel` objects with deterministic source paths;
- checks each expected program symbol, ELF section, and map name; and
- writes a manifest binding object SHA-256 values, target BTF SHA-256, ABI,
  program/attach names, architecture, and locked tool versions.

The loader must use the manifest-verified bytes exactly once to construct a
`BpfObjectContract`; it rejects substituted programs/maps and populates the
profile cgroup map before attaching a hook.  A profile with descendants must
already contain every resolved descendant cgroup ID at attach time.  A new or
recreated cgroup is not silently added; it requires the configured restart.

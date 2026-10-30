# v0.10.0 release readiness implementation plan

Goal: fail closed before publishing when evidence or required assets are absent.
Scope: release tooling only; no tag creation, release execution or merge.
Initial base: `14082d16b5e21f920acd2dc0b84ebd42f32ac859`.
Rebased inspection and implementation onto current dev
`78b224088bfbdb3ae0a0560c7f85a4fb0d055485` after concurrent consolidation.

- [x] Reproduce unsafe publication paths and add negative tests for missing,
  skipped, wrong-commit CI evidence, incomplete bundles and altered checksums.
- [x] Add metadata/CI evidence and artifact validation commands. Require all
  existing full manual CI jobs on the exact tagged commit; local results cannot
  substitute for them. Package source once, validate archive layout and require
  both Linux targets. Experimental Android builds do not establish runtime support.
- [x] Make tag pushes validation-only; publication requires explicit dispatch,
  successful CI evidence, native packaged-binary smoke checks, and R2 credentials.
  Disable the unguarded Make publication shortcut.
- [x] Implement one finalized bundle and mirror the GitHub-downloaded bytes, verify
  R2/public objects before promoting discovery pointers, serialize publication.
- [x] Run offline regressions, installer tests and syntax checks. Record unavailable
  Rust/platform/remote checks and release gates in a repository audit document.

Review focus: skipped checks reported as success; dispatch ref differs from tag;
experimental matrix failure; checksum regeneration hiding corruption; partial R2
upload or missing credentials. Tests must reject the first four; the upload script
must stop before discovery changes on any verification error.

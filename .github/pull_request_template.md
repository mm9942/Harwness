## Summary

<!-- One or two sentences: what does this change do, and why? -->

## Changes

<!-- The concrete changes, crate by crate if it spans more than one. -->

-

## Testing

<!-- Commands you ran and their result. Prefer focused checks while
     iterating; see CONTRIBUTING.md for the full list CI runs. -->

```bash

```

## Checklist

- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo clippy --workspace --tests -- -D warnings` passes
- [ ] Tests pass locally (`cargo nextest run --workspace` or `cargo test
      --workspace`), including doc tests (`cargo test --workspace --doc`)
- [ ] If this touches `dod/`: `cargo test --workspace --locked` passes there
- [ ] If this changes dependencies: `cargo deny check` passes
- [ ] Documentation (crate docs, `README.md`, `docs/`) is updated for any
      user-visible or configuration change
- [ ] If this is security-sensitive (approvals, sandboxing, `shell.exec`,
      `process.kill`, sudo handling, Defense-on-Device): the trust boundary
      and failure behavior are explained in **Summary**, and no invariant in
      [SECURITY.md](../SECURITY.md) is weakened

# Delta: `Cargo.lock` at `a856dea` is out of date

Found while building `harw` from `claude/r16-integration@a856dea`, with no
source change. `cargo build -p harw-cli --bin harw` rewrites `Cargo.lock`:

```diff
 name = "harw-oauth"
 dependencies = [
  "base64 0.23.1",
+ "fs4",
  "harw-config",
```

- **Cause:** `ripple-authz` (`6b340b4`, review item C) added the `fs4`
  dependency to `harw-oauth/Cargo.toml` but not its entry in the lockfile.
- **Effect:** every `--locked` step of the central build fails at this SHA,
  including `make -C dod clippy test`. Without `--locked`, cargo silently
  changes a tracked file during the build. The build would then run over a
  tree other than the frozen SHA, and `gate_record.py record` would refuse the
  dirty tree.
- **Fix:** commit the one-line `Cargo.lock` change on the integration branch
  before freezing: run `cargo update -p harw-oauth --offline`, or build once
  and commit the result.
- **Claude's side:** this was not committed. The integration branch is Harw's.
  The main checkout keeps the change only until the local `harw` build
  finishes.

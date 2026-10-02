# Mobile (experimental)

Status: experimental side branch `experimental/mobile-rust`. Not part of the
release line. The only workspace change is the new member `harw-mobile-core`,
registered in the root `Cargo.toml` and in `xtask/arch-policy.toml` (layer A),
so it is covered by the architecture gates; it has no consumers yet.

Goal: use the agent from a phone, Rust only (no Kotlin/Java UI code).

## Stages

1. **Termux + `harw attach` (no build).** The TUI is already Rust. Reach the
   gateway's `session.sock` through an SSH tunnel. No Kitty graphics on
   Android terminals; plain TUI only.
2. **Node listener composed (prerequisite).** `harw-config` has no node
   transport section yet, so `NodeListener` is not wired
   (see COMPOSITION-ROOT notes in docs/planning/65-cloud-sessions/CONSOLIDATION.md).
   Remote sessions must not reuse `EntryKind::Tui` rights: own entry kind or
   tier derived from `ClientIdentity` first.
3. **Rust GUI app (egui or Slint on android-activity).** Shares
   `harw-session-remote` (PQ node transport, `harw.session.v1`) with the TUI;
   only the UI is new. The phone is a device in the DeviceRegistry; approving
   from it requires the per-device opt-in (decision PL-68 §13).

## Not in scope

- Kitty graphics/keyboard protocol as a transport (a desktop TUI renderer
  option only).
- Secret-store login (YubiKey / Ledger Stax) for the host root session: later.

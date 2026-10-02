# Mobile (experimental)

Status: experimental side branch `experimental/mobile-rust`. Not part of the
release line. The only workspace change is the new member `harw-mobile-core`,
registered in the root `Cargo.toml` and in `xtask/arch-policy.toml` (layer A),
so it is covered by the architecture gates; it has no consumers yet.

Goal: use the agent from a phone, Rust only (no Kotlin/Java UI code).

## Stages

1. **Termux + `harw-mobile` (built).** A line-oriented client in Rust with
   no TUI dependency, so it runs in any phone terminal, including Termux:
   `harw-mobile [--socket PATH] [--session ID | --new]`. The default socket
   is the one `harw gateway --session-socket` and `harw attach` use. Type to
   send a prompt; `/y [n]` and `/n [n] [why]` decide the numbered open
   approvals, `/p` lists them, `/stop` interrupts, `/q` leaves. Reach a
   gateway on another machine by forwarding its socket over SSH. No Kitty
   graphics on Android terminals; plain text only.
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

## What exists

- `harw-mobile-core`: UI-agnostic view model (chat, approvals, turn activity,
  resume cursor) and a `Controller` over any `SessionPort`. Tested against
  the real session daemon over a Unix socket.
- `harw-mobile-term`: the `harw-mobile` binary: command parser, short
  plain-ASCII renderer, the loop. Tested end to end against the real daemon
  and smoke-tested as a binary.

Not yet: reconnect with backoff (S07, `ws/s07-reconnect-alias`), the node
listener for remote access, the Rust GUI.

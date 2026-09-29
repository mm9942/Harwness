//! Werkzeugoberfläche des Tunnel-Crates (Gerüst).
//!
//! v1-Vertrag (`docs/design/tunnel-policy-v1.md`): `tunnel.start` /
//! `tunnel.status` / `tunnel.stop` / `tunnel.list` — Loopback-only,
//! Ziel-Allowlist, Approval beim Start, Keyfile-Referenz statt
//! Key-Inhalt. Implementierung folgt im Plan-Knoten `job-lifecycle`
//! (Job-/WorkDriver-Anbindung, begrenzter Retry-/Backoff-Auto-Reconnect)
//! und `secrets-docs` (Secret-Redaktion).

#![forbid(unsafe_code)]

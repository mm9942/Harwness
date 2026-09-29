//! `harw-tool-tunnel` — verwaltete SSH-Portforwards (Tunnel-Werkzeug).
//!
//! Vertrag: `docs/design/tunnel-policy-v1.md` (Policy-Vertrag des Plans
//! `harw-tool-tunnel-v1`). v1-Scope:
//!
//! - nur **lokale** Forwards (`ssh -L`), Loopback-only-Bindung
//!   (127.0.0.1/::1, nie 0.0.0.0),
//! - Ziel-Allowlist (Hosts/Ports); private Ziele nur mit expliziter
//!   Erlaubnis, sonst verboten (Default-Verbot),
//! - Approval beim ersten Start bzw. bei Allowlist-Änderung,
//! - Keyfile-**Referenz** statt Key-Inhalt: der private Schlüssel wird nie
//!   als Argument, Log oder Artefakt weitergegeben (Secret-Redaktion),
//! - begrenzter Retry-/Backoff-Auto-Reconnect als expliziter Vertrag
//!   (max Versuche, Backoff-Policy, erschöpft = sichtbarer Fehlerstatus);
//!   kein eigener Daemon — Integration über den Job-/WorkDriver-Lifecycle
//!   (`harw-tool-job`).
//!
//! # Werkzeuge (geplant, v1)
//! - `tunnel.start` — startet einen verwalteten lokalen Forward (Job-basiert)
//! - `tunnel.status` — Status/Liveness eines Tunnels
//! - `tunnel.stop` — kontrollierter Abbruch
//! - `tunnel.list` — Übersicht der verwalteten Tunnels
//!
//! # Grenzen v1
//! Kein Remote-Forward (`-R`), kein SOCKS (`-D`), kein eigener Daemon.
//!
//! # Status
//! Gerüst (Plan-Knoten `crate-registry`): Registrierung in
//! `harw-registry-defaults` (capability_catalog, profile) ist angelegt; die
//! Implementierung folgt in den Plan-Knoten `job-lifecycle` und
//! `secrets-docs`.

#![forbid(unsafe_code)]

pub mod tools;

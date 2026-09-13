//! Tamper-evident audit layer (spec §4). An append-only, hash-chained event
//! log ([`chain`]) with periodic ML-DSA-signed chain-head checkpoints
//! ([`checkpoint`]). Records are immutable once written and independently
//! verifiable without trusting the process that wrote them.
//!
//! [`mirror`] adds a second, hostexternen Beobachtungsweg (AW7-04): weil ein
//! Audit auf demselben Host, den es überwacht, gegen eine Host-Übernahme
//! nichts beweist, trägt [`mirror::ChainAuditMirror`] den Kettenzustand über
//! einen [`mirror::MirrorTransport`] nach außen und meldet einen erkannten
//! Bruch dort — inhaltsfrei, siehe Moduldoku dort.
//!
//! [`telemetry`] macht [`mirror::ChainAuditMirror`]s internen
//! Kettenbruch-Zähler zusätzlich unter dem Telemetrienamen
//! [`telemetry::AUDIT_CHAIN_BREAK`] auffindbar — additiv, siehe dessen
//! Moduldoku für die ehrliche Einschränkung, was er beobachten kann.

pub mod chain;
pub mod checkpoint;
pub mod event;
pub mod mirror;
pub mod telemetry;

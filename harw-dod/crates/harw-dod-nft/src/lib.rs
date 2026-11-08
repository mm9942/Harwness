//! nftables im eigenen Prozess: eine kleine, prüfbare Kodierung der
//! nf_tables-Batches und ihr Transport über `NETLINK_NETFILTER`, mit denen
//! der Warden cgroups vom Netz trennt (PL-100 Entscheidung O10).
//!
//! # Warum diese Crate
//! O10 verlangt nftables „über eine Rust-Implementierung statt des
//! `nft`-Subprozesses“. Der Subprozess braucht im Warden `execve` in der
//! seccomp-Allowlist, `CAP_NET_ADMIN` in der Ambient-Menge und Lese-/
//! Ausführungsrechte für `nft`, Lader und Bibliotheken in Landlock. Die
//! Bewertung der Rust-nftables-Crates (Moduldoku von
//! `harw-dod/crates/harw-warden/src/isolation.rs`) fand keine tragbare
//! Crate und nennt die Alternative: „eine eigene, kleine Kodierung über
//! `rustix` und `NETLINK_NETFILTER`“. Das ist diese Crate. Sie kodiert genau
//! die drei Batches, die der Warden braucht — kein allgemeines nftables.
//!
//! # Aufbau
//! - **Kodierer** (rein, jede Plattform): [`CgroupRule`] prüft das Ziel,
//!   [`isolate_batch`], [`release_batch`] und [`release_all_batch`] erzeugen
//!   je ein Datagramm ([`Batch`]).
//! - **Antworten** (rein, Modul `reply`): Netlink-Rahmen zerlegen und
//!   entscheiden — Erfolg nur, wenn keine Antwort einen Fehler trägt, der
//!   Kernel jede Objektnachricht einzeln bestätigt hat und, falls er
//!   Batch-Nachrichten bestätigt (ab Linux 6.10), auch die Übernahme
//!   (`BATCH_END`).
//! - **Transport** (Modul `transport`, Socket nur unter Linux): ein Batch
//!   je Operation, Antworten mit Frist lesen.
//! - **API für `harw-warden`**: [`NftNetlink`] mit
//!   [`NftNetlink::isolate`], [`NftNetlink::release`],
//!   [`NftNetlink::release_chains`] und [`NftNetlink::release_all`];
//!   [`NETLINK_SYSCALLS`] nennt den Sandbox-Bedarf (nur Socket-Aufrufe,
//!   kein `execve`, keine Pfade).
//!
//! # Regelwerk
//! Tabelle `inet` [`TABLE_NAME`] (`harw_warden`); fremde Tabellen werden nie
//! berührt. Je cgroup zwei Basisketten `o_<suffix>` (`output`) und
//! `i_<suffix>` (`input`), Typ `filter`, Priorität 0, Policy `accept`, mit je
//! genau einer Regel `socket cgroupv2 level N <kernel_id> counter drop` —
//! dasselbe Regelwerk und dieselben Kettennamen wie die bisherigen
//! `nft`-Befehlszeilen von `harw-warden`, sodass Ketten aus dem alten
//! Backend vom neuen aufgehoben werden. Einzelheiten zu Nachrichten,
//! Idempotenz, Atomarität und Bestätigung: Modul `batch`.
//!
//! # Belege
//! - Jede Konstante ist mit Header und Zeile belegt (Modul `uapi`,
//!   `/usr/include/linux/{netlink.h, netfilter.h, netfilter/nfnetlink.h,
//!   netfilter/nf_tables.h}`, `/usr/include/asm-generic/errno*.h`).
//! - Die Bytereihenfolge (nf_tables-`u32` Big Endian, cgroup-ID Host) und das
//!   Register (`NFT_REG_1`) sind gegen einen `strace`-Mitschnitt von `nft`
//!   1.0.9 belegt; ein Test vergleicht den Kodierer Byte für Byte mit diesem
//!   Mitschnitt.
//! - Kernel 6.18 hat genau diese Kodierung in einem eigenen
//!   Netzwerk-Namespace angenommen: Isolieren (auch wiederholt), Aufheben
//!   (auch ohne vorheriges Isolieren), Alles-Aufheben (auch ohne Tabelle);
//!   Ebene 256 und ein Kettenname mit 256 Zeichen werden mit `ERANGE`
//!   abgelehnt. Antwortverhalten ebenfalls mit Kernel 6.18 belegt: je
//!   Nachricht eine Bestätigung, `BATCH_BEGIN` zuerst und auch bei einem
//!   verworfenen Batch, `BATCH_END` nur nach der Übernahme; `EPERM` ohne
//!   `CAP_NET_ADMIN` als einzige Antwort gegen `BATCH_BEGIN`; `EPERM` gegen
//!   `NEWTABLE`, wenn die Tabelle mit `NFT_TABLE_F_OWNER` einem anderen
//!   Socket gehört; ein Batch ohne `BATCH_END` wird ohne Fehlermeldung
//!   verworfen; Antworten synchron im `sendto`.
//! - Die selteneren Antwortformen sind am Kernelquelltext belegt
//!   (`net/netfilter/nfnetlink.c` v6.9 bis v6.18), nicht geprobt: eine
//!   gescheiterte Übernahme und ein gescheitertes Sammeln kommen gegen die
//!   erste Objektnachricht, nicht gegen `BATCH_BEGIN`; Linux 6.10 bis 6.17
//!   verschieben nach einer Wiederholung (Modul nachgeladen) die Bestätigung
//!   von `BATCH_BEGIN` auf die auslösende Nachricht (Modul `reply`,
//!   „Antwortformen des Kernels“).
//!
//! # Laufzeitvoraussetzungen
//! - Kernel ≥ 5.13 (`socket cgroupv2`); cgroup v2.
//! - `CAP_NET_ADMIN` in der wirksamen Menge des sendenden Threads, bei
//!   **jeder** Operation (Einzelheiten: [`NftNetlink`], „Rechte“). Der Warden
//!   öffnet den Socket erst, wenn die Capability wirksam ist, und gibt sie
//!   danach nie mehr ab.
//! - Der Socket wird im **Netzwerk-Namespace des Hosts** geöffnet: die
//!   Regeln landen im Namespace des Sockets (`sock_net`), nicht in dem des
//!   Aufrufers. Kein `PrivateNetwork=` und kein eigener Netzwerk-Namespace
//!   für den Warden.
//! - Der sendende Prozess läuft im **initialen cgroup-Namespace**: neuere
//!   Kernel (Probe: 6.18; nach Kenntnis ab 6.11) zählen `level` ab der
//!   Wurzel des cgroup-Namespace des Senders, ältere ab der echten Wurzel
//!   (Einzelheiten und Beleglage: [`CgroupRule`]). Nur im initialen
//!   Namespace stimmen beide überein; sonst trifft die Regel still nie. Die
//!   Crate prüft das nicht selbst.
//!
//! Der Kodierer selbst braucht nichts davon.
//!
//! # Fehler
//! [`NftError`], inhaltsfrei (keine Kernel-ID, kein Kettenname, keine
//! Antwortbytes); [`NftError::kind`] und `From<NftError> for io::Error` für
//! die Abbildung im Warden.
//!
//! # Nebenläufigkeit
//! Kodierer und Antwortleser: reine Funktionen, alles `Send + Sync`.
//! [`NftNetlink`]: `Send + Sync`, Operationen laufen nacheinander.
//!
//! # Examples
//! ```rust
//! use harw_dod_nft::{CgroupRule, isolate_batch, release_all_batch, release_batch};
//!
//! // Kernel-ID 4242 (Inode der cgroup), Tiefe 2 unter der cgroup2-Wurzel,
//! // Kettensuffix = Hex der cgroup-Kennung.
//! let rule = CgroupRule::new(4242, 2, "6a6f622d31")?;
//! let isolate = isolate_batch(&rule, 1)?;
//! let release = release_batch(&rule, isolate.end_seq().wrapping_add(1))?;
//! let all = release_all_batch(release.end_seq().wrapping_add(1))?;
//! assert_eq!(
//!     (isolate.message_count(), release.message_count(), all.message_count()),
//!     (9, 9, 4)
//! );
//! # Ok::<(), harw_dod_nft::NftError>(())
//! ```

#![forbid(unsafe_code)]

mod batch;
pub mod error;
mod netlink;
mod reply;
mod rule;
#[cfg(target_os = "linux")]
mod socket;
mod transport;
mod uapi;
mod wire;

#[cfg(test)]
mod golden_tests;
#[cfg(test)]
mod session_tests;
#[cfg(test)]
mod test_support;

pub use batch::{
    Batch, MAX_BATCH_LEN, TABLE_NAME, isolate_batch, release_all_batch, release_batch,
};
pub use error::{NftError, NftResult, SocketOp};
pub use netlink::{NETLINK_SYSCALLS, NetlinkSyscalls, NftNetlink, REPLY_TIMEOUT};
pub use rule::{CgroupRule, MAX_CHAIN_SUFFIX_LEN, MAX_SOCKET_LEVEL};

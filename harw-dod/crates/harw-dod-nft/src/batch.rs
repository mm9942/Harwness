//! nf_tables-Batches für Isolieren, Aufheben und Alles-Aufheben — reine
//! Funktionen von einer geprüften [`CgroupRule`] auf Bytes.
//!
//! # Verantwortungsbereich
//! [`isolate_batch`], [`release_batch`] und [`release_all_batch`] bauen je
//! **ein** Datagramm für `NETLINK_NETFILTER`: `NFNL_MSG_BATCH_BEGIN`, die
//! Objektnachrichten, `NFNL_MSG_BATCH_END`. Der Kernel wendet einen Batch
//! als eine Transaktion an — ganz oder gar nicht (Probe: ein scheiternder
//! Befehl am Ende lässt auch die vorher angelegte Tabelle nicht zurück).
//! Nichts hier öffnet einen Socket.
//!
//! # Regelwerk (identisch zu den bisherigen `nft`-Befehlszeilen)
//! Tabelle `inet` [`TABLE_NAME`], je cgroup zwei Basisketten (`o_<suffix>`
//! am `output`-, `i_<suffix>` am `input`-Hook, Typ `filter`, Priorität 0,
//! Policy `accept`) mit je genau einer Regel
//! `socket cgroupv2 level N <kernel_id> counter drop`.
//!
//! | Operation | Nachrichten im Batch |
//! |---|---|
//! | Isolieren | `NEWTABLE`; je Hook: `NEWCHAIN`, `DELRULE` (Kette leeren), `NEWRULE` |
//! | Aufheben | `NEWTABLE`; je Hook: `NEWCHAIN`, `DELRULE` (Kette leeren), `DELCHAIN` |
//! | Alles aufheben | `NEWTABLE`, `DELTABLE` |
//!
//! **Idempotent** ohne Vorab-Abfrage: `NEWTABLE`/`NEWCHAIN` tragen
//! `NLM_F_CREATE` ohne `NLM_F_EXCL` und scheitern deshalb nicht an
//! Vorhandenem; das Leeren vor `NEWRULE` verhindert doppelte Regeln; vor
//! jedem Löschen wird das Objekt angelegt, falls es fehlt, sodass kein
//! `ENOENT` den (atomaren) Batch kippt. `DELCHAIN` löscht eine Kette samt
//! Regeln auch ohne vorheriges Leeren (Probe, Kernel 6.18); das Leeren bleibt
//! für Gleichstand mit `nft` drin. `NFT_MSG_DESTROY*` (Löschen ohne
//! `ENOENT`) gibt es erst ab Kernel 6.3 und wird deshalb nicht benutzt;
//! `socket cgroupv2` braucht nur Kernel ≥ 5.13.
//!
//! **Kein Löschen ohne Namen:** `DELTABLE` ohne `NFTA_TABLE_NAME` leerte alle
//! Tabellen der Familie, `DELRULE` ohne `NFTA_RULE_CHAIN` alle Regeln der
//! Tabelle. Jede Löschnachricht hier trägt deshalb Tabellen- und
//! Kettennamen; die Tests prüfen das für jeden Batch.
//!
//! # Flags und Bestätigung
//! Jede Nachricht trägt `NLM_F_REQUEST`; dazu `NEWTABLE`/`NEWCHAIN`
//! `NLM_F_CREATE`, `NEWRULE` `NLM_F_CREATE | NLM_F_APPEND` (wie `nft` 1.0.9).
//! `NLM_F_ACK` tragen **alle Nachrichten**, auch `NFNL_MSG_BATCH_BEGIN` und
//! `NFNL_MSG_BATCH_END`. Was der Kernel darauf antwortet, ist am Quelltext
//! belegt (`net/netfilter/nfnetlink.c`, `nfnetlink_rcv_batch`; Zeilen v6.18,
//! verglichen mit v6.9 bis v6.17) und für die Normalfälle mit Kernel 6.18
//! geprobt (eigener Netzwerk-Namespace, `NETLINK_NETFILTER`-Socket); die
//! vollständige Liste steht im Modul `reply`, „Antwortformen des Kernels“:
//!
//! - Jede Objektnachricht mit `NLM_F_ACK` bekommt genau eine gesammelte
//!   Antwort: Fehlercode `0` oder ihren Fehler (`:538-561`). Zugestellt wird
//!   erst nach Übernahme bzw. Verwerfen des ganzen Batches (`:605`) — auch
//!   ein **verworfener** Batch bestätigt seine fehlerfreien Nachrichten mit
//!   `0` (Probe).
//! - Ab Linux 6.10 wird `BATCH_BEGIN` mit `0` bestätigt, als erste der
//!   gesammelten Antworten, auch bei einem verworfenen Batch (`:432-435`,
//!   Probe); `BATCH_END` nur nach erfolgreicher Übernahme, als letzte
//!   Antwort (`:583-586`, Probe). Linux 6.9 bestätigt keine der beiden
//!   (Quelltext), die Objektnachrichten aber schon.
//! - Linux 6.10 bis 6.17: musste der Kernel den Batch wiederholen, weil er
//!   ein Modul nachlädt (`-EAGAIN`), trägt die Bestätigung von
//!   `BATCH_BEGIN` Sequenznummer und Typ der auslösenden Nachricht. Beim
//!   ersten Isolieren nach dem Systemstart ist das typisch `NEWRULE` (der
//!   `socket`-Ausdruck lädt `nft_socket`). Erst v6.18 setzt den Kopf vorher
//!   auf `BATCH_BEGIN` zurück (`:379`, `:390`).
//! - Fehler vor der Verarbeitung meldet der Kernel sofort gegen
//!   `BATCH_BEGIN`, danach nichts: fehlendes `CAP_NET_ADMIN` (`:661-662`;
//!   Probe: `EPERM` als einzige Antwort, auch mit `NLM_F_ACK` an
//!   `BATCH_BEGIN`), fehlendes nf_tables (`EOPNOTSUPP`, `:406-419`).
//! - Fehler, die den ganzen Batch nach der Verarbeitung treffen, meldet er
//!   gegen die **erste Objektnachricht** (`NEWTABLE`), weil
//!   `nfnetlink_rcv_skb_batch` `BATCH_BEGIN` vorher vom Puffer abzieht
//!   (`:642`): eine gescheiterte Übernahme sofort, danach die gesammelten
//!   Antworten ohne `BATCH_END` (`:582`); ein gescheitertes Sammeln
//!   (`ENOMEM`) als einzige Antwort (`:549-551`).
//! - Ein Batch, in dem der Kernel `BATCH_END` nicht erreicht, wird ohne jede
//!   Fehlermeldung verworfen: Probe mit einem Batch ohne `BATCH_END` —
//!   Antworten `BATCH_BEGIN` `0` und `NEWTABLE` `0`, sonst nichts, die
//!   Tabelle entsteht nicht. Wer nur die Objektnachrichten zählte, hielte
//!   das für Erfolg.
//!
//! Daraus folgt die Regel im Modul `reply`: Erfolg verlangt eine
//! fehlerfreie Antwort auf jede Objektnachricht und keinen Fehler sonst;
//! zeigt der Kernel Batch-Bestätigungen (`BATCH_BEGIN` bestätigt, auch
//! verschoben), zusätzlich die Bestätigung von `BATCH_END`. Auf
//! älteren Kerneln bleibt nur die Regel je Objektnachricht; der stille
//! Abbruch ohne `BATCH_END` ist dort unerreichbar, weil jeder Batch dieser
//! Crate mit `BATCH_END` im selben Datagramm endet (die Tests prüfen das für
//! jeden Batch). Wer sich nur auf die Bestätigung von `BATCH_END` verließe,
//! hätte auf älteren Kerneln gar kein positives Signal und müsste Schweigen
//! als Erfolg werten; eine Isolation darf aber nie fälschlich als wirksam
//! gelten.
//!
//! # Sequenznummern
//! Nachricht `i` (Anfang = 0) trägt `first_seq + i`, modulo 2³²
//! (`wrapping_add`, bewusst: Netlink-Sequenznummern sind laufende Zähler).
//!
//! # Größe
//! Höchstens [`MAX_BATCH_LEN`] Byte. Der größte Batch (Isolieren mit dem
//! längsten Suffix) hat 2268 Byte; der Puffer wird einmal reserviert und
//! wächst nie darüber hinaus.
//!
//! # Nebenläufigkeit
//! Reine Funktionen; ein [`Batch`] ist unveränderlich und `Send + Sync`.
//!
//! # Examples
//! ```rust
//! use harw_dod_nft::{CgroupRule, isolate_batch, release_batch};
//!
//! let rule = CgroupRule::new(4242, 2, "6a6f622d31")?;
//! let batch = isolate_batch(&rule, 100)?;
//! assert_eq!(batch.message_count(), 9);
//! assert_eq!(batch.end_seq(), 108);
//! assert!(batch.contains_seq(104) && !batch.contains_seq(109));
//! assert!(release_batch(&rule, 200)?.as_bytes().len() <= harw_dod_nft::MAX_BATCH_LEN);
//! # Ok::<(), harw_dod_nft::NftError>(())
//! ```

use std::fmt;

use crate::error::{NftError, NftResult};
use crate::rule::{CgroupRule, Hook, chain_name};
use crate::uapi::{
    CHAIN_PRIORITY_FILTER, NF_ACCEPT, NF_DROP, NFNL_MSG_BATCH_BEGIN, NFNL_MSG_BATCH_END,
    NFNL_SUBSYS_NFTABLES, NFPROTO_INET, NFPROTO_UNSPEC, NFT_CMP_EQ, NFT_MSG_DELCHAIN,
    NFT_MSG_DELRULE, NFT_MSG_DELTABLE, NFT_MSG_NEWCHAIN, NFT_MSG_NEWRULE, NFT_MSG_NEWTABLE,
    NFT_REG_1, NFT_REG_VERDICT, NFT_SOCKET_CGROUPV2, NFTA_CHAIN_HOOK, NFTA_CHAIN_NAME,
    NFTA_CHAIN_POLICY, NFTA_CHAIN_TABLE, NFTA_CHAIN_TYPE, NFTA_CMP_DATA, NFTA_CMP_OP,
    NFTA_CMP_SREG, NFTA_DATA_VALUE, NFTA_DATA_VERDICT, NFTA_EXPR_DATA, NFTA_EXPR_NAME,
    NFTA_HOOK_HOOKNUM, NFTA_HOOK_PRIORITY, NFTA_IMMEDIATE_DATA, NFTA_IMMEDIATE_DREG,
    NFTA_LIST_ELEM, NFTA_RULE_CHAIN, NFTA_RULE_EXPRESSIONS, NFTA_RULE_TABLE, NFTA_SOCKET_DREG,
    NFTA_SOCKET_KEY, NFTA_SOCKET_LEVEL, NFTA_TABLE_FLAGS, NFTA_TABLE_NAME, NFTA_VERDICT_CODE,
    NLM_F_ACK, NLM_F_APPEND, NLM_F_CREATE, NLM_F_REQUEST,
};
use crate::wire::{MessageHeader, MessageStart, NestStart, NlWriter};

/// Name der eigenen nftables-Tabelle (Familie `inet`); keine andere Tabelle
/// wird berührt. Muss mit `NFT_TABLE_NAME` in `harw-warden`
/// (`isolation.rs`) übereinstimmen.
pub const TABLE_NAME: &str = "harw_warden";

/// Obergrenze eines kodierten Batches in Byte (eine Seite).
///
/// # Description
/// Der größte Batch dieser Crate hat 2268 Byte (Isolieren mit einem
/// Suffix von [`crate::MAX_CHAIN_SUFFIX_LEN`] Zeichen); die Tests belegen
/// das. Der Kodierer reserviert genau so viel und wächst nie darüber hinaus.
pub const MAX_BATCH_LEN: usize = 4096;

/// Kettentyp der Basisketten (`NFTA_CHAIN_TYPE`, `NLA_NUL_STRING`).
const CHAIN_TYPE_FILTER: &str = "filter";

/// Tabellen-Flags: keine (nicht `DORMANT`, nicht `OWNER`, siehe
/// `crate::uapi::NFTA_TABLE_FLAGS`).
const TABLE_FLAGS_NONE: u32 = 0;

/// Art einer Nachricht im Batch; ordnet Kernel-Antworten ihrer Nachricht
/// zu (über die Sequenznummer, gegengeprüft am kopierten Nachrichtentyp).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MessageKind {
    /// `NFNL_MSG_BATCH_BEGIN`.
    BatchBegin,
    /// `NFT_MSG_NEWTABLE`.
    NewTable,
    /// `NFT_MSG_DELTABLE`.
    DelTable,
    /// `NFT_MSG_NEWCHAIN`.
    NewChain,
    /// `NFT_MSG_DELCHAIN`.
    DelChain,
    /// `NFT_MSG_NEWRULE`.
    NewRule,
    /// `NFT_MSG_DELRULE` mit Kette, ohne Handle (Kette leeren).
    FlushChain,
    /// `NFNL_MSG_BATCH_END`.
    BatchEnd,
}

impl MessageKind {
    /// `nlmsg_type` dieser Nachricht.
    pub(crate) const fn nlmsg_type(self) -> u16 {
        match self {
            Self::BatchBegin => NFNL_MSG_BATCH_BEGIN,
            Self::BatchEnd => NFNL_MSG_BATCH_END,
            Self::NewTable => nft_msg_type(NFT_MSG_NEWTABLE),
            Self::DelTable => nft_msg_type(NFT_MSG_DELTABLE),
            Self::NewChain => nft_msg_type(NFT_MSG_NEWCHAIN),
            Self::DelChain => nft_msg_type(NFT_MSG_DELCHAIN),
            Self::NewRule => nft_msg_type(NFT_MSG_NEWRULE),
            Self::FlushChain => nft_msg_type(NFT_MSG_DELRULE),
        }
    }

    /// Ob die Nachricht etwas löscht (`DELTABLE`, `DELCHAIN`, `DELRULE`).
    pub(crate) const fn is_delete(self) -> bool {
        matches!(self, Self::DelTable | Self::DelChain | Self::FlushChain)
    }

    /// Ob die Nachricht ein Objekt ändert (alles außer Batch-Anfang/-Ende).
    pub(crate) const fn is_object(self) -> bool {
        !matches!(self, Self::BatchBegin | Self::BatchEnd)
    }
}

/// Ein fertig kodierter nf_tables-Batch für genau ein Datagramm.
///
/// # Description
/// Hält die Bytes, die Sequenznummern und die Art jeder Nachricht; `Debug`
/// zeigt nur Länge und Sequenznummern, nie die Bytes (sie enthalten
/// Kettennamen und Kernel-ID).
#[derive(Clone, PartialEq, Eq)]
pub struct Batch {
    bytes: Vec<u8>,
    first_seq: u32,
    message_count: u32,
    kinds: Vec<MessageKind>,
}

impl Batch {
    /// Die zu sendenden Bytes (ein Datagramm).
    ///
    /// # Returns
    /// Den Batch, höchstens [`MAX_BATCH_LEN`] Byte.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Sequenznummer von `NFNL_MSG_BATCH_BEGIN`.
    ///
    /// # Returns
    /// Die erste Sequenznummer des Batches.
    #[must_use]
    pub fn first_seq(&self) -> u32 {
        self.first_seq
    }

    /// Sequenznummer von `NFNL_MSG_BATCH_END`; neuere Kernel bestätigen sie
    /// nur nach erfolgreicher Übernahme (siehe Moduldoku).
    ///
    /// # Returns
    /// Die letzte Sequenznummer des Batches.
    #[must_use]
    pub fn end_seq(&self) -> u32 {
        self.first_seq
            .wrapping_add(self.message_count.saturating_sub(1))
    }

    /// Anzahl der Nachrichten samt Batch-Anfang und -Ende.
    ///
    /// # Returns
    /// Die Anzahl (Isolieren und Aufheben: 9, Alles aufheben: 4).
    #[must_use]
    pub fn message_count(&self) -> u32 {
        self.message_count
    }

    /// Ob `seq` zu einer Nachricht dieses Batches gehört (auch über den
    /// Überlauf von `u32` hinweg).
    ///
    /// # Arguments
    /// - `seq` (`u32`): Sequenznummer einer Kernel-Antwort.
    ///
    /// # Returns
    /// `true` für `first_seq ..= end_seq` (modulo 2³²).
    #[must_use]
    pub fn contains_seq(&self, seq: u32) -> bool {
        seq.wrapping_sub(self.first_seq) < self.message_count
    }

    /// Art der Nachricht mit Sequenznummer `seq`; `None` außerhalb des
    /// Batches.
    pub(crate) fn kind_of(&self, seq: u32) -> Option<MessageKind> {
        let offset = usize::try_from(seq.wrapping_sub(self.first_seq)).ok()?;
        self.kinds.get(offset).copied()
    }

    /// Die Arten aller Nachrichten in Sendereihenfolge.
    pub(crate) fn kinds(&self) -> &[MessageKind] {
        &self.kinds
    }
}

impl fmt::Debug for Batch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Batch")
            .field("len", &self.bytes.len())
            .field("first_seq", &self.first_seq)
            .field("message_count", &self.message_count)
            .finish()
    }
}

/// Kodiert das Isolieren einer cgroup.
///
/// # Description
/// `NEWTABLE`; je Hook (`output`, `input`): `NEWCHAIN` (Basiskette),
/// `DELRULE` (Kette leeren), `NEWRULE` (Drop-Regel). Idempotent.
///
/// # Arguments
/// - `rule` (`&CgroupRule`): das geprüfte Ziel.
/// - `first_seq` (`u32`): Sequenznummer des Batch-Anfangs.
///
/// # Returns
/// Den Batch (9 Nachrichten).
///
/// # Errors
/// [`NftError::Encoding`] nur bei verletzter Kodier-Invariante
/// (für geprüfte Regeln unerreichbar).
pub fn isolate_batch(rule: &CgroupRule, first_seq: u32) -> NftResult<Batch> {
    let mut builder = BatchBuilder::begin(first_seq)?;
    builder.new_table()?;
    for hook in Hook::BOTH {
        let chain = rule.chain_name(hook);
        builder.new_base_chain(&chain, hook)?;
        builder.flush_chain(&chain)?;
        builder.new_drop_rule(&chain, rule.kernel_id(), rule.level())?;
    }
    builder.finish()
}

/// Kodiert das Aufheben der Isolation einer cgroup.
///
/// # Description
/// `NEWTABLE`; je Hook: `NEWCHAIN` (falls sie fehlt), `DELRULE` (leeren),
/// `DELCHAIN`. Idempotent; berührt die Ketten anderer cgroups nicht. Die
/// (leere) Tabelle bleibt stehen.
///
/// # Arguments
/// - `rule` (`&CgroupRule`): das geprüfte Ziel (nur der Kettensuffix zählt).
/// - `first_seq` (`u32`): Sequenznummer des Batch-Anfangs.
///
/// # Returns
/// Den Batch (9 Nachrichten).
///
/// # Errors
/// [`NftError::Encoding`] nur bei verletzter Kodier-Invariante.
pub fn release_batch(rule: &CgroupRule, first_seq: u32) -> NftResult<Batch> {
    release_chains_batch(rule.chain_suffix(), first_seq)
}

/// Wie [`release_batch`], aber nur mit dem Kettensuffix.
///
/// # Description
/// Für das Aufheben ohne Kernel-ID (die cgroup kann schon fort sein). Der
/// Aufrufer hat `chain_suffix` mit `crate::rule::validate_chain_suffix`
/// geprüft.
///
/// # Errors
/// [`NftError::Encoding`] nur bei verletzter Kodier-Invariante.
pub(crate) fn release_chains_batch(chain_suffix: &str, first_seq: u32) -> NftResult<Batch> {
    let mut builder = BatchBuilder::begin(first_seq)?;
    builder.new_table()?;
    for hook in Hook::BOTH {
        let chain = chain_name(hook, chain_suffix);
        builder.new_base_chain(&chain, hook)?;
        builder.flush_chain(&chain)?;
        builder.delete_chain(&chain)?;
    }
    builder.finish()
}

/// Kodiert das Entfernen der ganzen eigenen Tabelle (jede Isolation fällt).
///
/// # Description
/// `NEWTABLE` (falls sie fehlt), `DELTABLE`. Idempotent.
///
/// # Arguments
/// - `first_seq` (`u32`): Sequenznummer des Batch-Anfangs.
///
/// # Returns
/// Den Batch (4 Nachrichten).
///
/// # Errors
/// [`NftError::Encoding`] nur bei verletzter Kodier-Invariante.
pub fn release_all_batch(first_seq: u32) -> NftResult<Batch> {
    let mut builder = BatchBuilder::begin(first_seq)?;
    builder.new_table()?;
    builder.delete_table()?;
    builder.finish()
}

/// `nlmsg_type` einer nf_tables-Nachricht: Subsystem im oberen Byte
/// (`NFNL_SUBSYS_ID`, `nfnetlink.h:46`).
const fn nft_msg_type(msg: u16) -> u16 {
    (NFNL_SUBSYS_NFTABLES << 8) | msg
}

/// Baut einen Batch Nachricht für Nachricht; vergibt die Sequenznummern
/// und merkt sich die Art jeder Nachricht.
pub(crate) struct BatchBuilder {
    writer: NlWriter,
    first_seq: u32,
    message_count: u32,
    kinds: Vec<MessageKind>,
}

impl BatchBuilder {
    /// Beginnt einen Batch mit `NFNL_MSG_BATCH_BEGIN` (`NLM_F_ACK`: neuere
    /// Kernel bestätigen ihn und zeigen damit, dass sie auch `BATCH_END`
    /// bestätigen; siehe Moduldoku).
    pub(crate) fn begin(first_seq: u32) -> NftResult<Self> {
        let mut builder = Self {
            writer: NlWriter::with_limit(MAX_BATCH_LEN),
            first_seq,
            message_count: 0,
            kinds: Vec::new(),
        };
        builder.control(MessageKind::BatchBegin, NLM_F_REQUEST | NLM_F_ACK)?;
        Ok(builder)
    }

    /// Schließt den Batch mit `NFNL_MSG_BATCH_END` (`NLM_F_ACK`).
    pub(crate) fn finish(mut self) -> NftResult<Batch> {
        self.control(MessageKind::BatchEnd, NLM_F_REQUEST | NLM_F_ACK)?;
        Ok(Batch {
            bytes: self.writer.into_bytes(),
            first_seq: self.first_seq,
            message_count: self.message_count,
            kinds: self.kinds,
        })
    }

    // Vergibt die nächste Sequenznummer und merkt sich die Art.
    fn next_seq(&mut self, kind: MessageKind) -> NftResult<u32> {
        let seq = self.first_seq.wrapping_add(self.message_count);
        self.message_count = self
            .message_count
            .checked_add(1)
            .ok_or(NftError::Encoding {
                reason: "too many messages in one batch",
            })?;
        self.kinds.push(kind);
        Ok(seq)
    }

    // Batch-Anfang/-Ende: Familie `NFPROTO_UNSPEC`, `res_id` =
    // `NFNL_SUBSYS_NFTABLES` (Big Endian), keine Attribute.
    fn control(&mut self, kind: MessageKind, flags: u16) -> NftResult<()> {
        let seq = self.next_seq(kind)?;
        let message = self.writer.begin_message(MessageHeader {
            msg_type: kind.nlmsg_type(),
            flags,
            seq,
            family: NFPROTO_UNSPEC,
            res_id: NFNL_SUBSYS_NFTABLES,
        })?;
        self.writer.end_message(message)
    }

    // Kopf einer Objektnachricht: Familie `inet`, `res_id` 0. Jede
    // Objektnachricht verlangt eine Antwort (`NLM_F_ACK`, siehe Moduldoku).
    fn object(&mut self, kind: MessageKind, flags: u16) -> NftResult<MessageStart> {
        let seq = self.next_seq(kind)?;
        self.writer.begin_message(MessageHeader {
            msg_type: kind.nlmsg_type(),
            flags: flags | NLM_F_ACK,
            seq,
            family: NFPROTO_INET,
            res_id: 0,
        })
    }

    /// `NFT_MSG_NEWTABLE`: Name, Flags 0; `NLM_F_CREATE` ohne `NLM_F_EXCL`.
    pub(crate) fn new_table(&mut self) -> NftResult<()> {
        let message = self.object(MessageKind::NewTable, NLM_F_REQUEST | NLM_F_CREATE)?;
        self.writer.attr_cstr(NFTA_TABLE_NAME, TABLE_NAME)?;
        self.writer.attr_be32(NFTA_TABLE_FLAGS, TABLE_FLAGS_NONE)?;
        self.writer.end_message(message)
    }

    /// `NFT_MSG_DELTABLE`: immer mit Namen (ohne Namen leerte der Kernel
    /// alle Tabellen der Familie).
    pub(crate) fn delete_table(&mut self) -> NftResult<()> {
        let message = self.object(MessageKind::DelTable, NLM_F_REQUEST)?;
        self.writer.attr_cstr(NFTA_TABLE_NAME, TABLE_NAME)?;
        self.writer.end_message(message)
    }

    /// `NFT_MSG_NEWCHAIN`: Basiskette `filter` an `hook`, Priorität 0,
    /// Policy `accept`. Attributreihenfolge wie `nft` 1.0.9.
    pub(crate) fn new_base_chain(&mut self, chain: &str, hook: Hook) -> NftResult<()> {
        let message = self.object(MessageKind::NewChain, NLM_F_REQUEST | NLM_F_CREATE)?;
        self.writer.attr_cstr(NFTA_CHAIN_TABLE, TABLE_NAME)?;
        self.writer.attr_cstr(NFTA_CHAIN_NAME, chain)?;
        self.writer.attr_be32(NFTA_CHAIN_POLICY, NF_ACCEPT)?;
        self.writer.attr_cstr(NFTA_CHAIN_TYPE, CHAIN_TYPE_FILTER)?;
        let hook_nest = self.writer.begin_nested(NFTA_CHAIN_HOOK)?;
        self.writer.attr_be32(NFTA_HOOK_HOOKNUM, hook.number())?;
        self.writer
            .attr_be32(NFTA_HOOK_PRIORITY, CHAIN_PRIORITY_FILTER)?;
        self.writer.end_nested(hook_nest)?;
        self.writer.end_message(message)
    }

    /// `NFT_MSG_DELRULE` mit Tabelle und Kette, ohne Handle: leert die
    /// Kette (ohne Kette leerte der Kernel die ganze Tabelle).
    pub(crate) fn flush_chain(&mut self, chain: &str) -> NftResult<()> {
        let message = self.object(MessageKind::FlushChain, NLM_F_REQUEST)?;
        self.writer.attr_cstr(NFTA_RULE_TABLE, TABLE_NAME)?;
        self.writer.attr_cstr(NFTA_RULE_CHAIN, chain)?;
        self.writer.end_message(message)
    }

    /// `NFT_MSG_DELCHAIN` mit Tabelle und Namen.
    pub(crate) fn delete_chain(&mut self, chain: &str) -> NftResult<()> {
        let message = self.object(MessageKind::DelChain, NLM_F_REQUEST)?;
        self.writer.attr_cstr(NFTA_CHAIN_TABLE, TABLE_NAME)?;
        self.writer.attr_cstr(NFTA_CHAIN_NAME, chain)?;
        self.writer.end_message(message)
    }

    /// `NFT_MSG_NEWRULE`: `socket cgroupv2 level <level>` → `NFT_REG_1`,
    /// `cmp eq` gegen die 8-Byte-ID (Host-Reihenfolge), `counter`,
    /// `immediate` → Verdict `NF_DROP`. Flags `CREATE | APPEND` wie `nft`
    /// (dazu `NLM_F_ACK` wie jede Objektnachricht).
    pub(crate) fn new_drop_rule(
        &mut self,
        chain: &str,
        kernel_id: u64,
        level: u32,
    ) -> NftResult<()> {
        let message = self.object(
            MessageKind::NewRule,
            NLM_F_REQUEST | NLM_F_CREATE | NLM_F_APPEND,
        )?;
        self.writer.attr_cstr(NFTA_RULE_TABLE, TABLE_NAME)?;
        self.writer.attr_cstr(NFTA_RULE_CHAIN, chain)?;
        let expressions = self.writer.begin_nested(NFTA_RULE_EXPRESSIONS)?;

        // socket: Schlüssel cgroupv2, Zielregister 1, Ebene.
        let data = self.begin_expression("socket")?;
        self.writer
            .attr_be32(NFTA_SOCKET_KEY, NFT_SOCKET_CGROUPV2)?;
        self.writer.attr_be32(NFTA_SOCKET_DREG, NFT_REG_1)?;
        self.writer.attr_be32(NFTA_SOCKET_LEVEL, level)?;
        self.end_expression(data)?;

        // cmp: Register 1 == kernel_id (8 Byte, Host-Reihenfolge).
        let data = self.begin_expression("cmp")?;
        self.writer.attr_be32(NFTA_CMP_SREG, NFT_REG_1)?;
        self.writer.attr_be32(NFTA_CMP_OP, NFT_CMP_EQ)?;
        let cmp_data = self.writer.begin_nested(NFTA_CMP_DATA)?;
        self.writer
            .attr_bytes(NFTA_DATA_VALUE, &kernel_id.to_ne_bytes())?;
        self.writer.end_nested(cmp_data)?;
        self.end_expression(data)?;

        // counter: ohne Startwerte (leeres Datennest, wie `nft`).
        let data = self.begin_expression("counter")?;
        self.end_expression(data)?;

        // immediate: Verdict-Register ← NF_DROP.
        let data = self.begin_expression("immediate")?;
        self.writer
            .attr_be32(NFTA_IMMEDIATE_DREG, NFT_REG_VERDICT)?;
        let immediate_data = self.writer.begin_nested(NFTA_IMMEDIATE_DATA)?;
        let verdict = self.writer.begin_nested(NFTA_DATA_VERDICT)?;
        self.writer.attr_be32(NFTA_VERDICT_CODE, NF_DROP)?;
        self.writer.end_nested(verdict)?;
        self.writer.end_nested(immediate_data)?;
        self.end_expression(data)?;

        self.writer.end_nested(expressions)?;
        self.writer.end_message(message)
    }

    // Öffnet `NFTA_LIST_ELEM { NFTA_EXPR_NAME, NFTA_EXPR_DATA { … } }` und
    // gibt beide offenen Nester zurück (außen, innen).
    fn begin_expression(&mut self, name: &str) -> NftResult<(NestStart, NestStart)> {
        let element = self.writer.begin_nested(NFTA_LIST_ELEM)?;
        self.writer.attr_cstr(NFTA_EXPR_NAME, name)?;
        let data = self.writer.begin_nested(NFTA_EXPR_DATA)?;
        Ok((element, data))
    }

    // Schließt die Nester aus `begin_expression` (innen, dann außen).
    fn end_expression(&mut self, (element, data): (NestStart, NestStart)) -> NftResult<()> {
        self.writer.end_nested(data)?;
        self.writer.end_nested(element)
    }
}

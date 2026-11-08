//! Konstanten der Linux-UAPI, die Batch-Kodierer und Antwortleser brauchen
//! — jede mit Header, Zeile und Wert belegt.
//!
//! # Verantwortungsbereich
//! Quelle sind die Kernel-UAPI-Header des Paketes `linux-libc-dev`
//! 6.8.0-147 unter `/usr/include/linux/`. Kein Wert stammt aus dem
//! Gedächtnis. Die Werte sind stabiles Kernel-ABI (Enum-Positionen und
//! `#define`s werden nie umnummeriert); die Zeilenangaben dienen dem
//! Nachprüfen im Review. Nur die Konstanten, die Kodierer und Leser
//! tatsächlich benutzen, stehen hier (keine toten Konstanten); die
//! Fehlercodes stammen aus `/usr/include/asm-generic/`.
//!
//! # Bytereihenfolge (aus den Headern nicht ablesbar, belegt)
//! - `struct nlmsghdr` und `struct nlattr` sind Host-Reihenfolge
//!   (`__u32`/`__u16`, `netlink.h:52-58`, `:225-228`).
//! - `nfgenmsg.res_id` ist `__be16` (`netfilter/nfnetlink.h:37`).
//! - Alle numerischen nf_tables-Attribute (`NLA_U32` in den Header-Kommentaren)
//!   liest der Kernel als Netzreihenfolge (Big Endian). Belegt durch den
//!   `strace`-Mitschnitt von `nft` 1.0.9 (Tabellen-Flags, Kettenpolicy,
//!   Hooknummer, Priorität, Socket-Schlüssel/-Register/-Ebene,
//!   Cmp-Register/-Operator, Verdict-Code, Immediate-Register: alle BE32) und
//!   durch das Annehmen genau dieser Kodierung durch Kernel 6.18 in einem
//!   eigenen Netzwerk-Namespace.
//! - Ausnahme: der Vergleichswert der cgroup-ID (`NFTA_DATA_VALUE`, 8 Byte)
//!   ist **Host**-Reihenfolge. Der Socket-Ausdruck kopiert die 64-Bit-ID
//!   unverändert in das Register; `nft` 1.0.9 kodiert ID 1 auf x86_64 als
//!   `01 00 00 00 00 00 00 00` und zeigt beim Debug-Dump
//!   `cmp eq reg 1 0x00000001 0x00000000`.
//!
//! # Nebenläufigkeit
//! Nur Konstanten.

// --- linux/netlink.h --------------------------------------------------------

/// `NLM_F_REQUEST` = 0x01 (`netlink.h:62`): Anfrage an den Kernel.
pub(crate) const NLM_F_REQUEST: u16 = 0x01;
/// `NLM_F_ACK` = 0x04 (`netlink.h:64`): Kernel antwortet mit `NLMSG_ERROR`
/// (Fehlercode `0` = Bestätigung).
pub(crate) const NLM_F_ACK: u16 = 0x04;
/// `NLM_F_CREATE` = 0x400 (`netlink.h:78`): anlegen, falls nicht vorhanden.
/// Ohne `NLM_F_EXCL` (0x200, `:77`) ist ein vorhandenes Objekt kein Fehler.
pub(crate) const NLM_F_CREATE: u16 = 0x400;
/// `NLM_F_APPEND` = 0x800 (`netlink.h:79`): Regel ans Kettenende.
pub(crate) const NLM_F_APPEND: u16 = 0x800;
/// `NLMSG_ALIGNTO` = 4 (`netlink.h:98`): Ausrichtung jeder Nachricht.
pub(crate) const NLMSG_ALIGNTO: usize = 4;
/// `NLA_ALIGNTO` = 4 (`netlink.h:244`): Ausrichtung jedes Attributs.
pub(crate) const NLA_ALIGNTO: usize = 4;
/// `NLA_F_NESTED` = 1 << 15 (`netlink.h:240`): Attribut trägt Attribute.
pub(crate) const NLA_F_NESTED: u16 = 1 << 15;
/// `NLMSG_ERROR` = 0x2 (`netlink.h:113`): Fehler- bzw. Bestätigungsantwort
/// (`struct nlmsgerr`, Fehlercode `0` = Bestätigung).
pub(crate) const NLMSG_ERROR: u16 = 0x2;
/// `NLMSG_HDRLEN` = `NLMSG_ALIGN(sizeof(struct nlmsghdr))` = 16
/// (`netlink.h:100`; `struct nlmsghdr` = `__u32 + __u16 + __u16 + __u32 +
/// __u32`, `:52-58`).
pub(crate) const NLMSG_HDRLEN: usize = 16;
/// Größe von `struct nlmsgerr` = `int error` + `struct nlmsghdr msg` = 20
/// (`netlink.h:119-131`). Der Kernel schickt mindestens diese 20 Byte: bei
/// einer Bestätigung genau den kopierten Kopf, bei einem Fehler danach noch
/// die ganze fehlgeschlagene Nachricht (ohne `NETLINK_CAP_ACK`, `:172`).
pub(crate) const NLMSGERR_LEN: usize = 20;

// --- linux/netfilter/nfnetlink.h -------------------------------------------

/// `NFNL_SUBSYS_NFTABLES` = 10 (`nfnetlink.h:61`): Subsystem im oberen Byte
/// des Nachrichtentyps und `res_id` von Batch-Anfang und -Ende.
pub(crate) const NFNL_SUBSYS_NFTABLES: u16 = 10;
/// `NFNL_MSG_BATCH_BEGIN` = `NLMSG_MIN_TYPE` = 0x10 (`nfnetlink.h:67`,
/// `netlink.h:117`).
pub(crate) const NFNL_MSG_BATCH_BEGIN: u16 = 0x10;
/// `NFNL_MSG_BATCH_END` = `NLMSG_MIN_TYPE + 1` = 0x11 (`nfnetlink.h:68`).
pub(crate) const NFNL_MSG_BATCH_END: u16 = 0x11;
/// `NFNETLINK_V0` = 0 (`nfnetlink.h:40`): `nfgenmsg.version`.
pub(crate) const NFNETLINK_V0: u8 = 0;

// --- linux/netfilter.h -------------------------------------------------------

/// `NF_DROP` = 0 (`netfilter.h:11`): Verdict der Isolationsregel.
pub(crate) const NF_DROP: u32 = 0;
/// `NF_ACCEPT` = 1 (`netfilter.h:12`): Policy der Basisketten.
pub(crate) const NF_ACCEPT: u32 = 1;
/// `NF_INET_LOCAL_IN` = 1 (`netfilter.h:42`, `enum nf_inet_hooks`): Hook
/// `input`.
pub(crate) const NF_INET_LOCAL_IN: u32 = 1;
/// `NF_INET_LOCAL_OUT` = 3 (`netfilter.h:44`): Hook `output`.
pub(crate) const NF_INET_LOCAL_OUT: u32 = 3;
/// `NFPROTO_UNSPEC` = 0 (`netfilter.h:57`): `nfgen_family` von Batch-Anfang
/// und -Ende.
pub(crate) const NFPROTO_UNSPEC: u8 = 0;
/// `NFPROTO_INET` = 1 (`netfilter.h:58`): Familie `inet` (IPv4 + IPv6).
pub(crate) const NFPROTO_INET: u8 = 1;

// --- linux/netfilter_ipv4.h --------------------------------------------------

/// `NF_IP_PRI_FILTER` = 0 (`netfilter_ipv4.h:37`; `NF_IP6_PRI_FILTER` = 0,
/// `netfilter_ipv6.h:41`): Priorität `filter` der Basisketten, als `u32`
/// (Zweierkomplement, hier ohnehin `0`).
pub(crate) const CHAIN_PRIORITY_FILTER: u32 = 0;

// --- linux/netfilter/nf_tables.h ---------------------------------------------

/// `NFT_NAME_MAXLEN` = 256 = `NFT_CHAIN_MAXNAMELEN` (`nf_tables.h:5`, `:7`),
/// samt abschließendem NUL. Kernel 6.18 nimmt 255 Zeichen an und lehnt 256
/// mit `ERANGE` ab (eigene Probe im Netzwerk-Namespace).
pub(crate) const NFT_CHAIN_MAXNAMELEN: usize = 256;

/// `NFT_REG_VERDICT` = 0 (`nf_tables.h:23`, `enum nft_registers`).
pub(crate) const NFT_REG_VERDICT: u32 = 0;
/// `NFT_REG_1` = 1 (`nf_tables.h:24`). Laut Header-Kommentar
/// (`nf_tables.h:13-21`) bilden `NFT_REG_1`..`NFT_REG_4` weiterhin
/// 16-Byte-Bereiche ab; die 8-Byte-cgroup-ID passt hinein. `nft` 1.0.9
/// lädt sie genau dorthin (`socket load cgroupv2 => reg 1`).
pub(crate) const NFT_REG_1: u32 = 1;

/// `NFT_MSG_NEWTABLE` = 0 (`nf_tables.h:111`, `enum nf_tables_msg_types`).
pub(crate) const NFT_MSG_NEWTABLE: u16 = 0;
/// `NFT_MSG_DELTABLE` = 2 (`nf_tables.h:113`).
pub(crate) const NFT_MSG_DELTABLE: u16 = 2;
/// `NFT_MSG_NEWCHAIN` = 3 (`nf_tables.h:114`).
pub(crate) const NFT_MSG_NEWCHAIN: u16 = 3;
/// `NFT_MSG_DELCHAIN` = 5 (`nf_tables.h:116`).
pub(crate) const NFT_MSG_DELCHAIN: u16 = 5;
/// `NFT_MSG_NEWRULE` = 6 (`nf_tables.h:117`).
pub(crate) const NFT_MSG_NEWRULE: u16 = 6;
/// `NFT_MSG_DELRULE` = 8 (`nf_tables.h:119`).
pub(crate) const NFT_MSG_DELRULE: u16 = 8;

/// `NFTA_LIST_ELEM` = 1 (`nf_tables.h:155`).
pub(crate) const NFTA_LIST_ELEM: u16 = 1;
/// `NFTA_HOOK_HOOKNUM` = 1 (`nf_tables.h:170`, `NLA_U32`).
pub(crate) const NFTA_HOOK_HOOKNUM: u16 = 1;
/// `NFTA_HOOK_PRIORITY` = 2 (`nf_tables.h:171`, `NLA_U32`).
pub(crate) const NFTA_HOOK_PRIORITY: u16 = 2;

/// `NFTA_TABLE_NAME` = 1 (`nf_tables.h:201`, `NLA_STRING`).
pub(crate) const NFTA_TABLE_NAME: u16 = 1;
/// `NFTA_TABLE_FLAGS` = 2 (`nf_tables.h:202`, `NLA_U32`). Gesendet wird `0`:
/// weder `NFT_TABLE_F_DORMANT` (0x1) noch `NFT_TABLE_F_OWNER` (0x2,
/// `nf_tables.h:184-185`) — eine Owner-Tabelle verschwände mit dem Socket.
pub(crate) const NFTA_TABLE_FLAGS: u16 = 2;

/// `NFTA_CHAIN_TABLE` = 1 (`nf_tables.h:238`, `NLA_STRING`).
pub(crate) const NFTA_CHAIN_TABLE: u16 = 1;
/// `NFTA_CHAIN_NAME` = 3 (`nf_tables.h:240`, `NLA_STRING`).
pub(crate) const NFTA_CHAIN_NAME: u16 = 3;
/// `NFTA_CHAIN_HOOK` = 4 (`nf_tables.h:241`, `NLA_NESTED`).
pub(crate) const NFTA_CHAIN_HOOK: u16 = 4;
/// `NFTA_CHAIN_POLICY` = 5 (`nf_tables.h:242`, `NLA_U32`).
pub(crate) const NFTA_CHAIN_POLICY: u16 = 5;
/// `NFTA_CHAIN_TYPE` = 7 (`nf_tables.h:244`, `NLA_NUL_STRING`).
pub(crate) const NFTA_CHAIN_TYPE: u16 = 7;

/// `NFTA_RULE_TABLE` = 1 (`nf_tables.h:270`, `NLA_STRING`).
pub(crate) const NFTA_RULE_TABLE: u16 = 1;
/// `NFTA_RULE_CHAIN` = 2 (`nf_tables.h:271`, `NLA_STRING`).
pub(crate) const NFTA_RULE_CHAIN: u16 = 2;
/// `NFTA_RULE_EXPRESSIONS` = 4 (`nf_tables.h:273`, `NLA_NESTED`).
pub(crate) const NFTA_RULE_EXPRESSIONS: u16 = 4;

/// `NFTA_DATA_VALUE` = 1 (`nf_tables.h:507`, `NLA_BINARY`).
pub(crate) const NFTA_DATA_VALUE: u16 = 1;
/// `NFTA_DATA_VERDICT` = 2 (`nf_tables.h:508`, `NLA_NESTED`).
pub(crate) const NFTA_DATA_VERDICT: u16 = 2;
/// `NFTA_VERDICT_CODE` = 1 (`nf_tables.h:525`, `NLA_U32`).
pub(crate) const NFTA_VERDICT_CODE: u16 = 1;

/// `NFTA_EXPR_NAME` = 1 (`nf_tables.h:540`, `NLA_STRING`).
pub(crate) const NFTA_EXPR_NAME: u16 = 1;
/// `NFTA_EXPR_DATA` = 2 (`nf_tables.h:541`, `NLA_NESTED`).
pub(crate) const NFTA_EXPR_DATA: u16 = 2;

/// `NFTA_IMMEDIATE_DREG` = 1 (`nf_tables.h:554`, `NLA_U32`).
pub(crate) const NFTA_IMMEDIATE_DREG: u16 = 1;
/// `NFTA_IMMEDIATE_DATA` = 2 (`nf_tables.h:555`, `NLA_NESTED`).
pub(crate) const NFTA_IMMEDIATE_DATA: u16 = 2;

/// `NFT_CMP_EQ` = 0 (`nf_tables.h:666`, `enum nft_cmp_ops`).
pub(crate) const NFT_CMP_EQ: u32 = 0;
/// `NFTA_CMP_SREG` = 1 (`nf_tables.h:683`, `NLA_U32`).
pub(crate) const NFTA_CMP_SREG: u16 = 1;
/// `NFTA_CMP_OP` = 2 (`nf_tables.h:684`, `NLA_U32`).
pub(crate) const NFTA_CMP_OP: u16 = 2;
/// `NFTA_CMP_DATA` = 3 (`nf_tables.h:685`, `NLA_NESTED`).
pub(crate) const NFTA_CMP_DATA: u16 = 3;

/// `NFTA_SOCKET_KEY` = 1 (`nf_tables.h:1093`).
pub(crate) const NFTA_SOCKET_KEY: u16 = 1;
/// `NFTA_SOCKET_DREG` = 2 (`nf_tables.h:1094`).
pub(crate) const NFTA_SOCKET_DREG: u16 = 2;
/// `NFTA_SOCKET_LEVEL` = 3 (`nf_tables.h:1095`, „cgroups2 ancestor level
/// (only for cgroupsv2)“).
pub(crate) const NFTA_SOCKET_LEVEL: u16 = 3;
/// `NFT_SOCKET_CGROUPV2` = 3 (`nf_tables.h:1112`, `enum nft_socket_keys`).
pub(crate) const NFT_SOCKET_CGROUPV2: u32 = 3;

// --- asm-generic/errno-base.h, asm-generic/errno.h ---------------------------

// Fehlercodes in `struct nlmsgerr.error` (negiert) und aus Socket-Aufrufen.
// Die Werte gelten für alle Architekturen, für die harw gebaut wird
// (`x86_64`, `aarch64`: beide `asm-generic`); ein Linux-Test vergleicht sie
// mit `rustix::io::Errno` der Zielarchitektur.

/// `EPERM` = 1 (`asm-generic/errno-base.h:5`): `nfnetlink_rcv` ohne
/// `CAP_NET_ADMIN` (Probe: eine Antwort auf `BATCH_BEGIN`).
pub(crate) const EPERM: i32 = 1;
/// `ENOENT` = 2 (`asm-generic/errno-base.h:6`).
pub(crate) const ENOENT: i32 = 2;
/// `EACCES` = 13 (`asm-generic/errno-base.h:17`).
pub(crate) const EACCES: i32 = 13;
/// `EEXIST` = 17 (`asm-generic/errno-base.h:21`).
pub(crate) const EEXIST: i32 = 17;
/// `EPROTONOSUPPORT` = 93 (`asm-generic/errno.h:76`): Kernel ohne
/// `NETLINK_NETFILTER`.
pub(crate) const EPROTONOSUPPORT: i32 = 93;
/// `EOPNOTSUPP` = 95 (`asm-generic/errno.h:78`): Kernel ohne nf_tables.
pub(crate) const EOPNOTSUPP: i32 = 95;
/// `EAFNOSUPPORT` = 97 (`asm-generic/errno.h:80`): Kernel ohne `AF_NETLINK`.
pub(crate) const EAFNOSUPPORT: i32 = 97;
/// `ENOBUFS` = 105 (`asm-generic/errno.h:88`).
pub(crate) const ENOBUFS: i32 = 105;

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use rustix::io::Errno;

    use super::{
        EACCES, EAFNOSUPPORT, EEXIST, ENOBUFS, ENOENT, EOPNOTSUPP, EPERM, EPROTONOSUPPORT,
    };
    use crate::test_support::TestResult;

    // The asm-generic values must equal the errno numbering of the target
    // architecture (rustix takes them from linux-raw-sys for that target).
    #[test]
    fn test_errno_values_match_the_target_architecture() -> TestResult {
        let pairs = [
            (EPERM, Errno::PERM),
            (ENOENT, Errno::NOENT),
            (EACCES, Errno::ACCESS),
            (EEXIST, Errno::EXIST),
            (EPROTONOSUPPORT, Errno::PROTONOSUPPORT),
            (EOPNOTSUPP, Errno::OPNOTSUPP),
            (EAFNOSUPPORT, Errno::AFNOSUPPORT),
            (ENOBUFS, Errno::NOBUFS),
        ];
        for (ours, theirs) in pairs {
            assert_eq!(ours, theirs.raw_os_error(), "{theirs:?}");
        }
        Ok(())
    }
}

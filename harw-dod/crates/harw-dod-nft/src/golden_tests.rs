//! Golden- und Eigenschaftstests des Batch-Kodierers.
//!
//! # Unabhängigkeit
//! Die erwarteten Bytes entstehen hier **nicht** über `crate::wire` oder
//! `crate::uapi`, sondern über einen eigenen, absichtlich schlichten
//! Test-Kodierer (Modul `expect`) mit erneut aus den Headern abgeschriebenen
//! Konstanten (jede mit Header und Zeile). Zusätzlich vergleicht ein Test den
//! Kodierer Byte für Byte mit einem `strace`-Mitschnitt von `nft` 1.0.9 —
//! einer völlig fremden Implementierung.
//!
//! Die Eigenschaftstests zerlegen jeden erzeugten Batch mit einem eigenen
//! Leser (Nachrichtenlängen, Ausrichtung, Füllbytes, Verschachtelung,
//! Sequenznummern, Flags) über alle zulässigen Suffixlängen.

#[cfg(target_endian = "little")]
use crate::batch::BatchBuilder;
use crate::batch::{MessageKind, release_chains_batch};
#[cfg(target_endian = "little")]
use crate::rule::Hook;
use crate::test_support::{TestError, TestResult, ctx, missing};
use crate::{
    Batch, CgroupRule, MAX_BATCH_LEN, MAX_CHAIN_SUFFIX_LEN, isolate_batch, release_all_batch,
    release_batch,
};

// --- Konstanten, erneut aus den Headern abgeschrieben ------------------------

/// `NLM_F_REQUEST` (`netlink.h:62`).
const REQUEST: u16 = 0x01;
/// `NLM_F_ACK` (`netlink.h:64`).
const ACK: u16 = 0x04;
/// `NLM_F_CREATE` (`netlink.h:78`).
const CREATE: u16 = 0x400;
/// `NLM_F_APPEND` (`netlink.h:79`).
const APPEND: u16 = 0x800;
/// `NLA_F_NESTED` (`netlink.h:240`).
const NESTED: u16 = 0x8000;
/// `NFNL_MSG_BATCH_BEGIN` = `NLMSG_MIN_TYPE` (`nfnetlink.h:67`, `netlink.h:117`).
const BATCH_BEGIN: u16 = 0x10;
/// `NFNL_MSG_BATCH_END` (`nfnetlink.h:68`).
const BATCH_END: u16 = 0x11;
/// `NFNL_SUBSYS_NFTABLES` (`nfnetlink.h:61`).
const SUBSYS_NFTABLES: u16 = 10;
/// `(10 << 8) | NFT_MSG_NEWTABLE (0, nf_tables.h:111)`.
const MSG_NEWTABLE: u16 = 0x0a00;
/// `(10 << 8) | NFT_MSG_DELTABLE (2, nf_tables.h:113)`.
const MSG_DELTABLE: u16 = 0x0a02;
/// `(10 << 8) | NFT_MSG_NEWCHAIN (3, nf_tables.h:114)`.
const MSG_NEWCHAIN: u16 = 0x0a03;
/// `(10 << 8) | NFT_MSG_DELCHAIN (5, nf_tables.h:116)`.
const MSG_DELCHAIN: u16 = 0x0a05;
/// `(10 << 8) | NFT_MSG_NEWRULE (6, nf_tables.h:117)`.
const MSG_NEWRULE: u16 = 0x0a06;
/// `(10 << 8) | NFT_MSG_DELRULE (8, nf_tables.h:119)`.
const MSG_DELRULE: u16 = 0x0a08;
/// `NFPROTO_UNSPEC` (`netfilter.h:57`).
const FAMILY_UNSPEC: u8 = 0;
/// `NFPROTO_INET` (`netfilter.h:58`).
const FAMILY_INET: u8 = 1;
/// `NFTA_TABLE_NAME` (`nf_tables.h:201`); ebenso `NFTA_CHAIN_TABLE`
/// (`:238`) und `NFTA_RULE_TABLE` (`:270`).
const ATTR_TABLE: u16 = 1;
/// `NFTA_TABLE_FLAGS` (`nf_tables.h:202`).
const ATTR_TABLE_FLAGS: u16 = 2;
/// `NFTA_RULE_CHAIN` (`nf_tables.h:271`).
const ATTR_RULE_CHAIN: u16 = 2;
/// `NFTA_CHAIN_NAME` (`nf_tables.h:240`).
const ATTR_CHAIN_NAME: u16 = 3;
/// `NFTA_CHAIN_HOOK` (`nf_tables.h:241`).
const ATTR_CHAIN_HOOK: u16 = 4;
/// `NFTA_CHAIN_POLICY` (`nf_tables.h:242`).
const ATTR_CHAIN_POLICY: u16 = 5;
/// `NFTA_CHAIN_TYPE` (`nf_tables.h:244`).
const ATTR_CHAIN_TYPE: u16 = 7;
/// `NFTA_RULE_EXPRESSIONS` (`nf_tables.h:273`).
const ATTR_RULE_EXPRESSIONS: u16 = 4;
/// `NF_INET_LOCAL_OUT` (`netfilter.h:44`).
const HOOK_OUTPUT: u32 = 3;
/// `NF_INET_LOCAL_IN` (`netfilter.h:42`).
const HOOK_INPUT: u32 = 1;

/// Längster Batch (Isolieren, Suffix mit 252 Zeichen), aus den Längen der
/// neun Nachrichten: 20 + 44 + 2 × (336 + 296 + 460) + 20.
const LONGEST_ISOLATE_LEN: usize = 2268;
/// Längster Aufhebe-Batch: 20 + 44 + 2 × (336 + 296 + 296) + 20.
const LONGEST_RELEASE_LEN: usize = 1940;

/// Ein Suffix für die Golden-Tests (Hex von `job-1`).
const SUFFIX: &str = "6a6f622d31";

// --- Unabhängiger Test-Kodierer ------------------------------------------------

mod expect {
    use super::{
        ACK, APPEND, BATCH_BEGIN, BATCH_END, CREATE, FAMILY_INET, FAMILY_UNSPEC, MSG_DELCHAIN,
        MSG_DELRULE, MSG_DELTABLE, MSG_NEWCHAIN, MSG_NEWRULE, MSG_NEWTABLE, NESTED, REQUEST,
        SUBSYS_NFTABLES,
    };
    use crate::test_support::{TestResult, missing};

    /// `struct nlattr` + Nutzlast + Nullfüllung auf 4; `nla_len` ohne Füllung.
    pub(super) fn attr(attr_type: u16, payload: &[u8]) -> TestResult<Vec<u8>> {
        let nla_len = payload
            .len()
            .checked_add(4)
            .and_then(|len| u16::try_from(len).ok())
            .ok_or_else(missing("nla_len"))?;
        let mut out = Vec::new();
        out.extend_from_slice(&nla_len.to_ne_bytes());
        out.extend_from_slice(&attr_type.to_ne_bytes());
        out.extend_from_slice(payload);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        Ok(out)
    }

    /// Verschachteltes Attribut über bereits ausgerichtete Kinder.
    pub(super) fn nest(attr_type: u16, children: &[Vec<u8>]) -> TestResult<Vec<u8>> {
        attr(attr_type | NESTED, &children.concat())
    }

    /// Zeichenkette samt NUL.
    pub(super) fn cstr(value: &str) -> Vec<u8> {
        let mut out = value.as_bytes().to_vec();
        out.push(0);
        out
    }

    /// `NLA_U32` in Netzreihenfolge.
    pub(super) fn be32(value: u32) -> [u8; 4] {
        value.to_be_bytes()
    }

    /// `nlmsghdr` (Host-Reihenfolge, pid 0) + `nfgenmsg` + Attribute.
    pub(super) fn message(
        msg_type: u16,
        flags: u16,
        seq: u32,
        family: u8,
        res_id: u16,
        attrs: &[Vec<u8>],
    ) -> TestResult<Vec<u8>> {
        let body = attrs.concat();
        let len = body
            .len()
            .checked_add(20)
            .and_then(|len| u32::try_from(len).ok())
            .ok_or_else(missing("nlmsg_len"))?;
        let mut out = Vec::new();
        out.extend_from_slice(&len.to_ne_bytes());
        out.extend_from_slice(&msg_type.to_ne_bytes());
        out.extend_from_slice(&flags.to_ne_bytes());
        out.extend_from_slice(&seq.to_ne_bytes());
        out.extend_from_slice(&0_u32.to_ne_bytes());
        out.push(family);
        out.push(0); // NFNETLINK_V0 (nfnetlink.h:40)
        out.extend_from_slice(&res_id.to_be_bytes());
        out.extend_from_slice(&body);
        Ok(out)
    }

    pub(super) fn begin(seq: u32) -> TestResult<Vec<u8>> {
        message(
            BATCH_BEGIN,
            REQUEST | ACK,
            seq,
            FAMILY_UNSPEC,
            SUBSYS_NFTABLES,
            &[],
        )
    }

    pub(super) fn end(seq: u32) -> TestResult<Vec<u8>> {
        message(
            BATCH_END,
            REQUEST | ACK,
            seq,
            FAMILY_UNSPEC,
            SUBSYS_NFTABLES,
            &[],
        )
    }

    fn table() -> TestResult<Vec<u8>> {
        attr(1, &cstr("harw_warden"))
    }

    pub(super) fn new_table(seq: u32) -> TestResult<Vec<u8>> {
        let attrs = [table()?, attr(2, &be32(0))?];
        message(
            MSG_NEWTABLE,
            REQUEST | CREATE | ACK,
            seq,
            FAMILY_INET,
            0,
            &attrs,
        )
    }

    pub(super) fn del_table(seq: u32) -> TestResult<Vec<u8>> {
        message(
            MSG_DELTABLE,
            REQUEST | ACK,
            seq,
            FAMILY_INET,
            0,
            &[table()?],
        )
    }

    pub(super) fn new_chain(seq: u32, chain: &str, hooknum: u32) -> TestResult<Vec<u8>> {
        let hook = nest(4, &[attr(1, &be32(hooknum))?, attr(2, &be32(0))?])?;
        let attrs = [
            table()?,
            attr(3, &cstr(chain))?,
            attr(5, &be32(1))?, // NF_ACCEPT
            attr(7, &cstr("filter"))?,
            hook,
        ];
        message(
            MSG_NEWCHAIN,
            REQUEST | CREATE | ACK,
            seq,
            FAMILY_INET,
            0,
            &attrs,
        )
    }

    pub(super) fn flush_chain(seq: u32, chain: &str) -> TestResult<Vec<u8>> {
        let attrs = [table()?, attr(2, &cstr(chain))?];
        message(MSG_DELRULE, REQUEST | ACK, seq, FAMILY_INET, 0, &attrs)
    }

    pub(super) fn del_chain(seq: u32, chain: &str) -> TestResult<Vec<u8>> {
        let attrs = [table()?, attr(3, &cstr(chain))?];
        message(MSG_DELCHAIN, REQUEST | ACK, seq, FAMILY_INET, 0, &attrs)
    }

    fn expr(name: &str, data: &[Vec<u8>]) -> TestResult<Vec<u8>> {
        nest(1, &[attr(1, &cstr(name))?, nest(2, data)?])
    }

    pub(super) fn new_rule(
        seq: u32,
        chain: &str,
        kernel_id: u64,
        level: u32,
    ) -> TestResult<Vec<u8>> {
        // socket: KEY = NFT_SOCKET_CGROUPV2 (3), DREG = NFT_REG_1 (1), LEVEL.
        let socket = expr(
            "socket",
            &[
                attr(1, &be32(3))?,
                attr(2, &be32(1))?,
                attr(3, &be32(level))?,
            ],
        )?;
        // cmp: SREG = 1, OP = NFT_CMP_EQ (0), DATA { VALUE = id (host order) }.
        let cmp = expr(
            "cmp",
            &[
                attr(1, &be32(1))?,
                attr(2, &be32(0))?,
                nest(3, &[attr(1, &kernel_id.to_ne_bytes())?])?,
            ],
        )?;
        let counter = expr("counter", &[])?;
        // immediate: DREG = NFT_REG_VERDICT (0), DATA { VERDICT { CODE = NF_DROP (0) } }.
        let immediate = expr(
            "immediate",
            &[
                attr(1, &be32(0))?,
                nest(2, &[nest(2, &[attr(1, &be32(0))?])?])?,
            ],
        )?;
        let attrs = [
            table()?,
            attr(2, &cstr(chain))?,
            nest(4, &[socket, cmp, counter, immediate])?,
        ];
        message(
            MSG_NEWRULE,
            REQUEST | CREATE | APPEND | ACK,
            seq,
            FAMILY_INET,
            0,
            &attrs,
        )
    }
}

// --- Unabhängiger Leser ---------------------------------------------------------

/// Eine zerlegte Netlink-Nachricht.
#[derive(Debug)]
struct Message<'a> {
    bytes: &'a [u8],
    msg_type: u16,
    flags: u16,
    seq: u32,
    pid: u32,
    family: u8,
    version: u8,
    res_id: u16,
    attrs: &'a [u8],
}

/// Ein zerlegtes Attribut (Typ ohne Flag-Bits).
#[derive(Debug)]
struct Attr<'a> {
    attr_type: u16,
    nested: bool,
    payload: &'a [u8],
}

fn take<const N: usize>(input: &mut &[u8]) -> TestResult<[u8; N]> {
    let current: &[u8] = *input;
    let (head, rest) = current
        .split_first_chunk::<N>()
        .ok_or_else(missing("bytes"))?;
    *input = rest;
    Ok(*head)
}

fn split_messages(mut bytes: &[u8]) -> TestResult<Vec<Message<'_>>> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let mut cursor = bytes;
        let len = usize::try_from(u32::from_ne_bytes(take::<4>(&mut cursor)?))
            .map_err(ctx("nlmsg_len"))?;
        if len < 20 || !len.is_multiple_of(4) {
            return Err(TestError::Unexpected(format!("bad nlmsg_len {len}")));
        }
        let (message, rest) = bytes
            .split_at_checked(len)
            .ok_or_else(missing("whole message"))?;
        let msg_type = u16::from_ne_bytes(take::<2>(&mut cursor)?);
        let flags = u16::from_ne_bytes(take::<2>(&mut cursor)?);
        let seq = u32::from_ne_bytes(take::<4>(&mut cursor)?);
        let pid = u32::from_ne_bytes(take::<4>(&mut cursor)?);
        let [family, version] = take::<2>(&mut cursor)?;
        let res_id = u16::from_be_bytes(take::<2>(&mut cursor)?);
        let attrs = message.get(20..).ok_or_else(missing("attributes"))?;
        out.push(Message {
            bytes: message,
            msg_type,
            flags,
            seq,
            pid,
            family,
            version,
            res_id,
            attrs,
        });
        bytes = rest;
    }
    Ok(out)
}

fn split_attrs(mut bytes: &[u8]) -> TestResult<Vec<Attr<'_>>> {
    let mut out = Vec::new();
    while !bytes.is_empty() {
        let mut cursor = bytes;
        let len = usize::from(u16::from_ne_bytes(take::<2>(&mut cursor)?));
        let raw_type = u16::from_ne_bytes(take::<2>(&mut cursor)?);
        if len < 4 {
            return Err(TestError::Unexpected(format!("bad nla_len {len}")));
        }
        let padded = len.checked_next_multiple_of(4).ok_or_else(missing("pad"))?;
        let (whole, rest) = bytes
            .split_at_checked(padded)
            .ok_or_else(missing("padded attribute"))?;
        let payload = whole.get(4..len).ok_or_else(missing("payload"))?;
        let padding = whole.get(len..).ok_or_else(missing("padding"))?;
        if padding.iter().any(|byte| *byte != 0) {
            return Err(TestError::Unexpected("non-zero padding".to_owned()));
        }
        out.push(Attr {
            attr_type: raw_type & 0x3fff,
            nested: raw_type & NESTED != 0,
            payload,
        });
        bytes = rest;
    }
    Ok(out)
}

/// Prüft rekursiv jedes verschachtelte Attribut; liefert die Zahl der Attribute.
fn walk_tree(bytes: &[u8]) -> TestResult<usize> {
    let mut count = 0_usize;
    for attr in split_attrs(bytes)? {
        count = count.saturating_add(1);
        if attr.nested {
            count = count.saturating_add(walk_tree(attr.payload)?);
        }
    }
    Ok(count)
}

fn find<'a, 'b>(attrs: &'b [Attr<'a>], attr_type: u16) -> TestResult<&'b Attr<'a>> {
    attrs
        .iter()
        .find(|attr| attr.attr_type == attr_type)
        .ok_or_else(|| TestError::Unexpected(format!("attribute {attr_type} missing")))
}

fn chains(suffix: &str) -> (String, String) {
    (format!("o_{suffix}"), format!("i_{suffix}"))
}

fn expected_isolate(
    first_seq: u32,
    suffix: &str,
    kernel_id: u64,
    level: u32,
) -> TestResult<Vec<Vec<u8>>> {
    let (output, input) = chains(suffix);
    let seq = |offset: u32| first_seq.wrapping_add(offset);
    Ok(vec![
        expect::begin(seq(0))?,
        expect::new_table(seq(1))?,
        expect::new_chain(seq(2), &output, HOOK_OUTPUT)?,
        expect::flush_chain(seq(3), &output)?,
        expect::new_rule(seq(4), &output, kernel_id, level)?,
        expect::new_chain(seq(5), &input, HOOK_INPUT)?,
        expect::flush_chain(seq(6), &input)?,
        expect::new_rule(seq(7), &input, kernel_id, level)?,
        expect::end(seq(8))?,
    ])
}

fn expected_release(first_seq: u32, suffix: &str) -> TestResult<Vec<Vec<u8>>> {
    let (output, input) = chains(suffix);
    let seq = |offset: u32| first_seq.wrapping_add(offset);
    Ok(vec![
        expect::begin(seq(0))?,
        expect::new_table(seq(1))?,
        expect::new_chain(seq(2), &output, HOOK_OUTPUT)?,
        expect::flush_chain(seq(3), &output)?,
        expect::del_chain(seq(4), &output)?,
        expect::new_chain(seq(5), &input, HOOK_INPUT)?,
        expect::flush_chain(seq(6), &input)?,
        expect::del_chain(seq(7), &input)?,
        expect::end(seq(8))?,
    ])
}

fn expected_release_all(first_seq: u32) -> TestResult<Vec<Vec<u8>>> {
    let seq = |offset: u32| first_seq.wrapping_add(offset);
    Ok(vec![
        expect::begin(seq(0))?,
        expect::new_table(seq(1))?,
        expect::del_table(seq(2))?,
        expect::end(seq(3))?,
    ])
}

/// Vergleicht Nachricht für Nachricht (Fehler nennen den Index) und dann
/// den ganzen Batch.
fn assert_batch_eq(batch: &Batch, expected: &[Vec<u8>]) -> TestResult {
    let messages = split_messages(batch.as_bytes())?;
    assert_eq!(messages.len(), expected.len());
    for (index, (message, want)) in messages.iter().zip(expected).enumerate() {
        assert_eq!(message.bytes, want.as_slice(), "message {index}");
    }
    assert_eq!(batch.as_bytes(), expected.concat().as_slice());
    Ok(())
}

// --- Golden-Tests ----------------------------------------------------------------

#[test]
fn test_release_all_batch_golden() -> TestResult {
    let batch = release_all_batch(7)?;
    assert_batch_eq(&batch, &expected_release_all(7)?)?;
    assert_eq!(batch.message_count(), 4);
    assert_eq!((batch.first_seq(), batch.end_seq()), (7, 10));
    Ok(())
}

#[test]
fn test_isolate_batch_golden() -> TestResult {
    let rule = CgroupRule::new(0x0102_0304_0506_0708, 3, SUFFIX)?;
    let batch = isolate_batch(&rule, 100)?;
    assert_batch_eq(
        &batch,
        &expected_isolate(100, SUFFIX, 0x0102_0304_0506_0708, 3)?,
    )?;
    assert_eq!(batch.message_count(), 9);
    assert_eq!(batch.end_seq(), 108);
    Ok(())
}

#[test]
fn test_release_batch_golden() -> TestResult {
    let rule = CgroupRule::new(42, 1, SUFFIX)?;
    let batch = release_batch(&rule, 200)?;
    assert_batch_eq(&batch, &expected_release(200, SUFFIX)?)?;
    assert_eq!(batch.message_count(), 9);
    assert_eq!(batch.end_seq(), 208);
    Ok(())
}

#[test]
fn test_batch_begin_and_end_carry_the_nftables_resource_id() -> TestResult {
    let batch = release_all_batch(1)?;
    let messages = split_messages(batch.as_bytes())?;
    let first = messages.first().ok_or_else(missing("begin"))?;
    let last = messages.last().ok_or_else(missing("end"))?;
    for (message, msg_type, flags) in [
        (first, BATCH_BEGIN, REQUEST | ACK),
        (last, BATCH_END, REQUEST | ACK),
    ] {
        assert_eq!(message.bytes.len(), 20);
        assert_eq!(message.msg_type, msg_type);
        assert_eq!(message.flags, flags);
        assert_eq!(message.family, FAMILY_UNSPEC);
        assert_eq!(message.version, 0);
        assert_eq!(message.res_id, SUBSYS_NFTABLES);
        assert!(message.attrs.is_empty());
    }
    Ok(())
}

#[cfg(target_endian = "little")]
#[test]
fn test_kernel_id_is_encoded_in_host_order() -> TestResult {
    let rule = CgroupRule::new(0x0102_0304_0506_0708, 2, "6a")?;
    let batch = isolate_batch(&rule, 1)?;
    let messages = split_messages(batch.as_bytes())?;
    let new_rule = messages
        .iter()
        .find(|message| message.msg_type == MSG_NEWRULE)
        .ok_or_else(missing("NEWRULE"))?;
    let little_endian = [8_u8, 7, 6, 5, 4, 3, 2, 1];
    let found = new_rule
        .attrs
        .windows(little_endian.len())
        .any(|window| window == little_endian);
    assert!(found, "cgroup id must appear in host (little-endian) order");
    let big_endian = [1_u8, 2, 3, 4, 5, 6, 7, 8];
    assert!(
        !new_rule
            .attrs
            .windows(big_endian.len())
            .any(|window| window == big_endian)
    );
    Ok(())
}

/// `strace` von `nft -f` (nftables 1.0.9, x86_64, Kernel 6.18, eigener
/// Netzwerk-Namespace) für dieses Skript:
///
/// ```text
/// add table inet harw_warden
/// add chain inet harw_warden o_probe { type filter hook output priority 0 ; policy accept ; }
/// flush chain inet harw_warden o_probe
/// add rule inet harw_warden o_probe socket cgroupv2 level 2 "unified" counter drop
/// ```
///
/// Das ganze `sendmsg`-Datagramm (432 Byte; cgroup-ID 1, Ebene 2), aus der
/// `strace`-Dekodierung Byte für Byte zurückgewonnen.
#[cfg(target_endian = "little")]
const NFT_1_0_9_CAPTURE: [&str; 14] = [
    "140000001000010000000000000000000000000a2c000000000a010001000000",
    "000000000100000010000100686172775f77617264656e000800020000000000",
    "58000000030a010402000000000000000100000010000100686172775f776172",
    "64656e000c0003006f5f70726f62650008000500000000010b00070066696c74",
    "65720000140004800800010000000003080002000000000030000000080a0100",
    "03000000000000000100000010000100686172775f77617264656e000c000200",
    "6f5f70726f626500d4000000060a010c04000000000000000100000010000100",
    "686172775f77617264656e000c0002006f5f70726f626500a40004802c000180",
    "0b000100736f636b657400001c00028008000100000000030800020000000001",
    "08000300000000023000018008000100636d7000240002800800010000000001",
    "0800020000000000100003800c0001000100000000000000140001800c000100",
    "636f756e7465720004000280300001800e000100696d6d656469617465000000",
    "1c0002800800010000000000100002800c000280080001000000000014000000",
    "1100010005000000000000000000000a",
];

#[cfg(target_endian = "little")]
fn decode_hex(lines: &[&str]) -> TestResult<Vec<u8>> {
    let mut out = Vec::new();
    for line in lines {
        for pair in line.as_bytes().chunks(2) {
            let text = std::str::from_utf8(pair).map_err(ctx("hex pair"))?;
            out.push(u8::from_str_radix(text, 16).map_err(ctx("hex digit"))?);
        }
    }
    Ok(out)
}

#[cfg(target_endian = "little")]
fn patch_u16(bytes: &mut [u8], at: usize, value: u16) -> TestResult {
    let end = at.checked_add(2).ok_or_else(missing("offset"))?;
    let target = bytes.get_mut(at..end).ok_or_else(missing("patch target"))?;
    target.copy_from_slice(&value.to_ne_bytes());
    Ok(())
}

/// Der Kodierer gegen `nft` 1.0.9: identisch bis auf dokumentierte Flags —
/// `NEWTABLE` trägt hier zusätzlich `NLM_F_CREATE` (idempotentes Anlegen
/// laut Auftrag), jede Nachricht samt `BATCH_BEGIN` und `BATCH_END` trägt
/// `NLM_F_ACK` (eine Antwort je Nachricht, Modul `batch`).
#[cfg(target_endian = "little")]
#[test]
fn test_encoder_matches_the_nft_1_0_9_capture_byte_for_byte() -> TestResult {
    let mut expected = decode_hex(&NFT_1_0_9_CAPTURE)?;
    assert_eq!(expected.len(), 432);
    // Nachrichtengrenzen des Mitschnitts: BEGIN 20, NEWTABLE 44, NEWCHAIN
    // 88, DELRULE 48, NEWRULE 212, END 20 Byte; Flags nach len(4)+type(2).
    let starts_and_flags = [
        (0_usize, REQUEST | ACK),               // BATCH_BEGIN (nft: REQUEST)
        (20, REQUEST | CREATE | ACK),           // NEWTABLE (nft: REQUEST)
        (64, REQUEST | CREATE | ACK),           // NEWCHAIN (nft: REQUEST|CREATE)
        (152, REQUEST | ACK),                   // DELRULE (nft: REQUEST)
        (200, REQUEST | CREATE | APPEND | ACK), // NEWRULE (nft: 0x0c01)
        (412, REQUEST | ACK),                   // BATCH_END (nft: REQUEST)
    ];
    for (start, flags) in starts_and_flags {
        let at = start.checked_add(6).ok_or_else(missing("flags offset"))?;
        patch_u16(&mut expected, at, flags)?;
    }

    let mut builder = BatchBuilder::begin(0)?;
    builder.new_table()?;
    builder.new_base_chain("o_probe", Hook::Output)?;
    builder.flush_chain("o_probe")?;
    builder.new_drop_rule("o_probe", 1, 2)?;
    let batch = builder.finish()?;

    let ours = split_messages(batch.as_bytes())?;
    let theirs = split_messages(&expected)?;
    assert_eq!(ours.len(), theirs.len());
    for (index, (mine, nft)) in ours.iter().zip(&theirs).enumerate() {
        assert_eq!(mine.bytes, nft.bytes, "message {index}");
    }
    assert_eq!(batch.as_bytes(), expected.as_slice());
    Ok(())
}

// --- Eigenschaftstests -------------------------------------------------------------

/// Struktur eines Batches: Längen, Ausrichtung, Sequenznummern, Flags,
/// Köpfe, rekursiv gültige Attribute; Rückgabe: die Nachrichtentypen.
fn check_structure(batch: &Batch) -> TestResult<Vec<u16>> {
    let bytes = batch.as_bytes();
    assert!(bytes.len() <= MAX_BATCH_LEN);
    let messages = split_messages(bytes)?;
    let count = u32::try_from(messages.len()).map_err(ctx("message count"))?;
    assert_eq!(count, batch.message_count());
    let mut total = 0_usize;
    let mut kinds = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        let offset = u32::try_from(index).map_err(ctx("index"))?;
        let seq = batch.first_seq().wrapping_add(offset);
        assert_eq!(message.seq, seq);
        assert!(batch.contains_seq(seq));
        assert_eq!(message.pid, 0);
        assert_eq!(message.version, 0);
        assert_ne!(message.flags & REQUEST, 0, "every message is a request");
        assert_ne!(message.flags & ACK, 0, "every message asks for an ACK");
        let is_first = index == 0;
        let is_last = index.saturating_add(1) == messages.len();
        assert_eq!(
            batch.kind_of(seq).map(MessageKind::nlmsg_type),
            Some(message.msg_type),
            "the batch knows the type of message {index}"
        );
        if is_first || is_last {
            assert_eq!(message.family, FAMILY_UNSPEC);
            assert_eq!(message.res_id, SUBSYS_NFTABLES);
            assert!(message.attrs.is_empty());
        } else {
            assert_eq!(message.family, FAMILY_INET);
            assert_eq!(message.res_id, 0);
            assert!(walk_tree(message.attrs)? >= 1);
            // Every object message names our table and nothing else.
            let attrs = split_attrs(message.attrs)?;
            assert_eq!(find(&attrs, ATTR_TABLE)?.payload, b"harw_warden\0");
        }
        total = total.saturating_add(message.bytes.len());
        kinds.push(message.msg_type);
    }
    assert_eq!(total, bytes.len());
    assert_eq!(
        batch.end_seq(),
        batch.first_seq().wrapping_add(count.saturating_sub(1))
    );
    assert!(!batch.contains_seq(batch.end_seq().wrapping_add(1)));
    assert!(!batch.contains_seq(batch.first_seq().wrapping_sub(1)));
    Ok(kinds)
}

#[test]
fn test_every_suffix_length_matches_golden_and_structure() -> TestResult {
    let seeds: [(u64, u32, u32); 3] = [
        (1, 1, 0),
        (u64::MAX, 255, u32::MAX.wrapping_sub(3)),
        (987_654_321, 7, 41),
    ];
    let hex = "0123456789abcdef";
    for length in (2..=MAX_CHAIN_SUFFIX_LEN).step_by(2) {
        let suffix: String = hex.chars().cycle().take(length).collect();
        for (kernel_id, level, first_seq) in seeds {
            let rule = CgroupRule::new(kernel_id, level, &suffix)?;
            let isolate = isolate_batch(&rule, first_seq)?;
            assert_batch_eq(
                &isolate,
                &expected_isolate(first_seq, &suffix, kernel_id, level)?,
            )?;
            let kinds = check_structure(&isolate)?;
            assert_eq!(
                kinds,
                [
                    BATCH_BEGIN,
                    MSG_NEWTABLE,
                    MSG_NEWCHAIN,
                    MSG_DELRULE,
                    MSG_NEWRULE,
                    MSG_NEWCHAIN,
                    MSG_DELRULE,
                    MSG_NEWRULE,
                    BATCH_END
                ]
            );
            let release = release_batch(&rule, first_seq)?;
            assert_batch_eq(&release, &expected_release(first_seq, &suffix)?)?;
            let kinds = check_structure(&release)?;
            assert_eq!(
                kinds,
                [
                    BATCH_BEGIN,
                    MSG_NEWTABLE,
                    MSG_NEWCHAIN,
                    MSG_DELRULE,
                    MSG_DELCHAIN,
                    MSG_NEWCHAIN,
                    MSG_DELRULE,
                    MSG_DELCHAIN,
                    BATCH_END
                ]
            );
        }
    }
    for first_seq in [0, 1, u32::MAX.wrapping_sub(2), u32::MAX] {
        let all = release_all_batch(first_seq)?;
        assert_batch_eq(&all, &expected_release_all(first_seq)?)?;
        let kinds = check_structure(&all)?;
        assert_eq!(kinds, [BATCH_BEGIN, MSG_NEWTABLE, MSG_DELTABLE, BATCH_END]);
    }
    Ok(())
}

#[test]
fn test_longest_batches_stay_below_the_bound() -> TestResult {
    let longest = "ab".repeat(MAX_CHAIN_SUFFIX_LEN / 2);
    let rule = CgroupRule::new(u64::MAX, 255, &longest)?;
    let isolate = isolate_batch(&rule, u32::MAX)?;
    let release = release_batch(&rule, u32::MAX)?;
    assert_eq!(isolate.as_bytes().len(), LONGEST_ISOLATE_LEN);
    assert_eq!(release.as_bytes().len(), LONGEST_RELEASE_LEN);
    const { assert!(LONGEST_ISOLATE_LEN <= MAX_BATCH_LEN) };
    assert_eq!(rule.output_chain().len(), 254);
    Ok(())
}

#[test]
fn test_delete_messages_always_name_their_target() -> TestResult {
    let rule = CgroupRule::new(9, 2, SUFFIX)?;
    let (output, input) = chains(SUFFIX);
    let batches = [
        isolate_batch(&rule, 1)?,
        release_batch(&rule, 1)?,
        release_all_batch(1)?,
    ];
    for batch in &batches {
        for message in split_messages(batch.as_bytes())? {
            let attrs = split_attrs(message.attrs)?;
            match message.msg_type {
                MSG_DELTABLE => {
                    // Without a name the kernel would flush every inet table.
                    assert_eq!(find(&attrs, ATTR_TABLE)?.payload, b"harw_warden\0");
                }
                MSG_DELRULE => {
                    // Without a chain the kernel would flush the whole table.
                    let chain = find(&attrs, ATTR_RULE_CHAIN)?.payload;
                    let named = [output.as_str(), input.as_str()]
                        .iter()
                        .any(|name| chain.strip_suffix(b"\0") == Some(name.as_bytes()));
                    assert!(named, "flush must name one of our chains");
                }
                MSG_DELCHAIN => {
                    let chain = find(&attrs, ATTR_CHAIN_NAME)?.payload;
                    let named = [output.as_str(), input.as_str()]
                        .iter()
                        .any(|name| chain.strip_suffix(b"\0") == Some(name.as_bytes()));
                    assert!(named, "delete must name one of our chains");
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[test]
fn test_table_is_created_without_owner_or_dormant_flags() -> TestResult {
    let batch = release_all_batch(5)?;
    let messages = split_messages(batch.as_bytes())?;
    let new_table = messages
        .iter()
        .find(|message| message.msg_type == MSG_NEWTABLE)
        .ok_or_else(missing("NEWTABLE"))?;
    assert_eq!(
        new_table.flags,
        REQUEST | CREATE | ACK,
        "CREATE without EXCL"
    );
    let attrs = split_attrs(new_table.attrs)?;
    assert_eq!(find(&attrs, ATTR_TABLE_FLAGS)?.payload, [0, 0, 0, 0]);
    Ok(())
}

#[test]
fn test_base_chains_hook_output_and_input_with_accept_policy() -> TestResult {
    let rule = CgroupRule::new(9, 2, SUFFIX)?;
    let batch = isolate_batch(&rule, 1)?;
    let mut hooks = Vec::new();
    for message in split_messages(batch.as_bytes())? {
        if message.msg_type != MSG_NEWCHAIN {
            continue;
        }
        assert_eq!(message.flags, REQUEST | CREATE | ACK);
        let attrs = split_attrs(message.attrs)?;
        assert_eq!(find(&attrs, ATTR_CHAIN_POLICY)?.payload, [0, 0, 0, 1]);
        assert_eq!(find(&attrs, ATTR_CHAIN_TYPE)?.payload, b"filter\0");
        let hook = find(&attrs, ATTR_CHAIN_HOOK)?;
        assert!(hook.nested);
        let hook_attrs = split_attrs(hook.payload)?;
        let number = find(&hook_attrs, 1)?.payload;
        let number: [u8; 4] = number.try_into().map_err(ctx("hooknum"))?;
        hooks.push(u32::from_be_bytes(number));
        assert_eq!(find(&hook_attrs, 2)?.payload, [0, 0, 0, 0]);
    }
    assert_eq!(hooks, [HOOK_OUTPUT, HOOK_INPUT]);
    Ok(())
}

#[test]
fn test_rule_has_exactly_socket_cmp_counter_immediate() -> TestResult {
    let rule = CgroupRule::new(9, 4, SUFFIX)?;
    let batch = isolate_batch(&rule, 1)?;
    let messages = split_messages(batch.as_bytes())?;
    let new_rule = messages
        .iter()
        .find(|message| message.msg_type == MSG_NEWRULE)
        .ok_or_else(missing("NEWRULE"))?;
    assert_eq!(new_rule.flags, REQUEST | CREATE | APPEND | ACK);
    let attrs = split_attrs(new_rule.attrs)?;
    let expressions = find(&attrs, ATTR_RULE_EXPRESSIONS)?;
    assert!(expressions.nested);
    let mut names = Vec::new();
    for element in split_attrs(expressions.payload)? {
        assert!(element.nested);
        assert_eq!(element.attr_type, 1); // NFTA_LIST_ELEM
        let inner = split_attrs(element.payload)?;
        let name = find(&inner, 1)?.payload; // NFTA_EXPR_NAME
        names.push(String::from_utf8_lossy(name).into_owned());
        assert!(find(&inner, 2)?.nested); // NFTA_EXPR_DATA
    }
    assert_eq!(names, ["socket\0", "cmp\0", "counter\0", "immediate\0"]);
    Ok(())
}

#[test]
fn test_sequence_numbers_wrap_around_u32() -> TestResult {
    let batch = release_all_batch(u32::MAX.wrapping_sub(1))?;
    assert_eq!(batch.first_seq(), u32::MAX.wrapping_sub(1));
    assert_eq!(batch.end_seq(), 1);
    assert!(batch.contains_seq(u32::MAX));
    assert!(batch.contains_seq(0));
    assert!(batch.contains_seq(1));
    assert!(!batch.contains_seq(2));
    assert!(!batch.contains_seq(u32::MAX.wrapping_sub(2)));
    let seqs: Vec<u32> = split_messages(batch.as_bytes())?
        .iter()
        .map(|message| message.seq)
        .collect();
    assert_eq!(seqs, [u32::MAX.wrapping_sub(1), u32::MAX, 0, 1]);
    Ok(())
}

#[test]
fn test_batch_debug_shows_no_bytes() -> TestResult {
    let rule = CgroupRule::new(424_242, 2, SUFFIX)?;
    let batch = isolate_batch(&rule, 3)?;
    let rendered = format!("{batch:?}");
    assert!(rendered.contains("len"));
    assert!(!rendered.contains(SUFFIX));
    assert!(!rendered.contains("424242"));
    Ok(())
}

#[test]
fn test_encoding_is_deterministic() -> TestResult {
    let rule = CgroupRule::new(77, 3, SUFFIX)?;
    assert_eq!(isolate_batch(&rule, 9)?, isolate_batch(&rule, 9)?);
    assert_ne!(isolate_batch(&rule, 9)?, isolate_batch(&rule, 10)?);
    Ok(())
}

#[test]
fn test_release_by_suffix_equals_release_by_rule() -> TestResult {
    let rule = CgroupRule::new(31, 2, SUFFIX)?;
    assert_eq!(release_chains_batch(SUFFIX, 77)?, release_batch(&rule, 77)?);
    Ok(())
}

#[test]
fn test_message_kinds_follow_the_batch() -> TestResult {
    let rule = CgroupRule::new(31, 2, SUFFIX)?;
    let batch = isolate_batch(&rule, u32::MAX)?;
    assert_eq!(
        batch.kinds(),
        [
            MessageKind::BatchBegin,
            MessageKind::NewTable,
            MessageKind::NewChain,
            MessageKind::FlushChain,
            MessageKind::NewRule,
            MessageKind::NewChain,
            MessageKind::FlushChain,
            MessageKind::NewRule,
            MessageKind::BatchEnd,
        ]
    );
    assert_eq!(batch.kind_of(u32::MAX), Some(MessageKind::BatchBegin));
    assert_eq!(batch.kind_of(7), Some(MessageKind::BatchEnd));
    assert_eq!(batch.kind_of(8), None);
    assert_eq!(batch.kind_of(u32::MAX.wrapping_sub(1)), None);
    let deletes: Vec<bool> = release_all_batch(1)?
        .kinds()
        .iter()
        .map(|kind| kind.is_delete())
        .collect();
    assert_eq!(deletes, [false, false, true, false]);
    let objects: Vec<bool> = release_all_batch(1)?
        .kinds()
        .iter()
        .map(|kind| kind.is_object())
        .collect();
    assert_eq!(objects, [false, true, true, false]);
    Ok(())
}

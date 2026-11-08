//! Geprüftes Ziel einer Isolationsregel: Kernel-ID, Ebene und Kettensuffix
//! einer cgroup ([`CgroupRule`]).
//!
//! # Verantwortungsbereich
//! [`CgroupRule::new`] ist die einzige Stelle, an der Eingaben geprüft
//! werden; der Kodierer ([`crate::isolate_batch`], [`crate::release_batch`])
//! nimmt nur geprüfte Regeln an. Nach dem Bau ist eine Regel unveränderlich.
//!
//! # Bedeutung der Felder
//! Der nf_tables-Ausdruck `socket cgroupv2 level N` lädt die ID der
//! **Vorfahren-cgroup auf Tiefe `N`** (gezählt ab der cgroup-v2-Wurzel = 0)
//! des Sockets in ein Register; die Regel vergleicht sie mit `kernel_id`.
//! Damit die Regel die Ziel-cgroup samt allen Kindern trifft, muss also
//! gelten:
//!
//! - `kernel_id` ist die cgroup-ID der Ziel-cgroup. Auf 64-Bit-Systemen ist
//!   das die Inode-Nummer ihres Verzeichnisses im cgroup2-Dateisystem
//!   (`stat`); so ermittelt sie auch `harw-dod-warden` (`FsTargetObserver`,
//!   `metadata.ino()`). Für die cgroup2-Wurzel (ID 1) belegt durch `nft`
//!   1.0.9; für eine tiefere cgroup durch eine Probe (Kernel 6.18,
//!   Hybrid-Modus, eigener Netzwerk-Namespace): eine Regel mit Inode und
//!   Ebene 1 einer frischen cgroup verwirft deren UDP ausgehend (`EPERM`
//!   beim Senden) und eingehend, fremder Verkehr bleibt frei; mit Ebene 2
//!   trifft dieselbe Regel nicht. Für Tiefe 2 belegt die Probe zum
//!   cgroup-Namespace unten dasselbe (Inode von `hp-a/hp-b`, über `nft`
//!   eingefügt). Der Integrationstest `tests/kernel.rs` (root, `#[ignore]`)
//!   wiederholt Tiefe 1 über [`crate::NftNetlink`].
//! - `level` ist die Tiefe der Ziel-cgroup unter der **cgroup2-Wurzel** —
//!   nicht unter `/sys/fs/cgroup`, falls cgroup2 woanders eingehängt ist
//!   (Hybrid-Modus: `/sys/fs/cgroup/unified`). Eine falsche Ebene ergibt
//!   eine Regel, die nie trifft — keine Isolation, aber auch kein Fehler.
//! - **Voraussetzung: der sendende Prozess läuft im initialen
//!   cgroup-Namespace.** Der Kernel addiert beim Anlegen der Regel die
//!   Ebene der Wurzel des cgroup-Namespace des Senders zu
//!   `NFTA_SOCKET_LEVEL`. Probe (Kernel 6.18, Hybrid-Modus, eigener
//!   Netzwerk-Namespace, cgroup `hp-a/hp-b` auf Tiefe 2): aus dem initialen
//!   cgroup-Namespace trifft Ebene 2, Ebene 1 nicht; aus einem
//!   cgroup-Namespace mit Wurzel `hp-a` trifft Ebene 1, Ebene 2 nicht —
//!   dieselbe Inode in allen vier Fällen. Eingeführt hat das nach Kenntnis
//!   Linux 6.11 („netfilter: nft_socket: make cgroupsv2 matching work with
//!   namespaces“; Version nicht am Kernelquelltext geprüft); ältere Kernel
//!   werten die Ebene absolut. Im initialen Namespace ist die Wurzel die
//!   echte Wurzel (Ebene 0), und alle Kernel deuten die Ebene gleich. In
//!   einem anderen Namespace (z. B. systemd `ProtectControlGroups=private`)
//!   träfe eine absolute Ebene auf neueren Kerneln nie, eine
//!   namespace-relative auf älteren nie — wieder still keine Isolation.
//!   Diese Crate prüft das nicht (sie liest keine Dateien); `harw-warden`
//!   soll es beim Start prüfen: Inode von `/proc/self/ns/cgroup` gleich
//!   `PROC_CGROUP_INIT_INO` = `0xEFFFFFFB` (Kernel `include/linux/proc_ns.h`,
//!   kein UAPI; auf dem Probe-Host 4026531835, in einem frischen
//!   cgroup-Namespace eine andere Zahl). Der Vergleich mit
//!   `/proc/1/ns/cgroup` taugt nicht: der Warden läuft nicht als root, und
//!   der Link von PID 1 ist ihm ohne `CAP_SYS_PTRACE` verwehrt (Probe). Die
//!   Kernel-ID ist davon nicht betroffen (sie ist in jedem Namespace
//!   dieselbe).
//! - `chain_suffix` benennt die beiden Basisketten (`o_<suffix>` am
//!   `output`-, `i_<suffix>` am `input`-Hook). `harw-warden` nimmt dafür
//!   heute die Hex-Kodierung der cgroup-Kennung (`NftCgroupTarget`); dieselbe
//!   Benennung hält das Aufheben unabhängig davon, ob die cgroup noch
//!   existiert oder ihre Kernel-ID gewechselt hat.
//!
//! # Prüfregeln
//! - `kernel_id != 0` ([`NftError::InvalidKernelId`]).
//! - `1 <= level <=` [`MAX_SOCKET_LEVEL`] ([`NftError::InvalidLevel`]).
//!   Ebene 0 wäre die Wurzel-cgroup: die Regel nähme jedem Prozess des
//!   Hosts das Netz, und der Kernel nimmt sie an (Probe, Kernel 6.18).
//! - `chain_suffix`: nicht leer, gerade Länge (ganze Bytes), nur `0-9a-f`,
//!   höchstens [`MAX_CHAIN_SUFFIX_LEN`] Zeichen
//!   ([`NftError::InvalidChainSuffix`]). Damit ist jeder Kettenname ein
//!   gültiger, NUL-freier nf_tables-Name.
//!
//! # `Debug` ohne Inhalt
//! `Debug` zeigt nur die Ebene, nie Kernel-ID oder Kettensuffix (der Suffix
//! ist das Hex der cgroup-Kennung) — wie [`crate::Batch`], dessen Bytes
//! beides enthalten. Ein mit `?` geloggtes Ziel verrät die cgroup nicht.
//!
//! # Nebenläufigkeit
//! Unveränderlich nach dem Bau, `Send + Sync`.
//!
//! # Examples
//! ```rust
//! use harw_dod_nft::CgroupRule;
//!
//! let rule = CgroupRule::new(4242, 2, "6a6f622d31")?;
//! assert_eq!(rule.output_chain(), "o_6a6f622d31");
//! assert_eq!(rule.input_chain(), "i_6a6f622d31");
//! assert!(CgroupRule::new(4242, 0, "6a6f622d31").is_err());
//! # Ok::<(), harw_dod_nft::NftError>(())
//! ```

use std::fmt;
use std::num::NonZeroU64;

use crate::error::{NftError, NftResult};
use crate::uapi::{NF_INET_LOCAL_IN, NF_INET_LOCAL_OUT, NFT_CHAIN_MAXNAMELEN};

/// Länge der Kettenpräfixe `o_` und `i_`.
const CHAIN_PREFIX_LEN: usize = 2;

/// Längster zulässiger Kettensuffix in Zeichen: 252.
///
/// # Description
/// Ein Kettenname hat höchstens `NFT_CHAIN_MAXNAMELEN - 1` = 255 Zeichen
/// (`nf_tables.h:5`, `:7`; Kernel 6.18 lehnt 256 mit `ERANGE` ab). Abzüglich
/// des Präfixes bleiben 253; abgerundet auf ganze Bytes (gerade Länge) 252 —
/// genau die Hex-Länge der längsten cgroup-Kennung, die `harw-warden`
/// zulässt (`MAX_CGROUP_ID_LEN = 126`).
pub const MAX_CHAIN_SUFFIX_LEN: usize = {
    let longest = NFT_CHAIN_MAXNAMELEN - 1 - CHAIN_PREFIX_LEN;
    longest - longest % 2
};

/// Höchste zulässige cgroup-Ebene für `socket cgroupv2 level N`: 255.
///
/// # Description
/// Die UAPI-Header nennen keine Grenze; Kernel 6.18 nimmt 255 an und lehnt
/// 256 mit `ERANGE` ab (eigene Probe im Netzwerk-Namespace).
pub const MAX_SOCKET_LEVEL: u32 = 255;

/// Netfilter-Hook einer der beiden Basisketten je cgroup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hook {
    /// `output` (`NF_INET_LOCAL_OUT`): ausgehender Verkehr der cgroup.
    Output,
    /// `input` (`NF_INET_LOCAL_IN`): eingehender Verkehr zu ihren Sockets.
    Input,
}

impl Hook {
    /// Beide Hooks in Batch-Reihenfolge (erst `output`, dann `input` — wie
    /// die bisherigen `nft`-Befehlszeilen von `harw-warden`).
    pub(crate) const BOTH: [Self; 2] = [Self::Output, Self::Input];

    /// Hooknummer für `NFTA_HOOK_HOOKNUM`.
    pub(crate) const fn number(self) -> u32 {
        match self {
            Self::Output => NF_INET_LOCAL_OUT,
            Self::Input => NF_INET_LOCAL_IN,
        }
    }

    // Präfix des Kettennamens (`o_`/`i_`, wie `NftCgroupTarget` in
    // `harw-warden`).
    const fn chain_prefix(self) -> &'static str {
        match self {
            Self::Output => "o_",
            Self::Input => "i_",
        }
    }
}

/// Geprüftes Ziel einer Isolationsregel.
///
/// # Description
/// - `kernel_id`: cgroup-ID der Ziel-cgroup (Inode ihres
///   cgroup2-Verzeichnisses, nie `0`).
/// - `level`: Tiefe der Ziel-cgroup unter der echten cgroup2-Wurzel
///   (`1..=255`). **Voraussetzung:** wer die Regel sendet
///   ([`crate::NftNetlink`]), läuft im initialen cgroup-Namespace; neuere
///   Kernel (Probe: 6.18) zählen die Ebene sonst ab der Wurzel seines
///   Namespace, und die Regel träfe still nie.
/// - `chain_suffix`: Kleinbuchstaben-Hex-Stamm der Kettennamen.
///
/// `Debug` zeigt nur die Ebene (keine Kernel-ID, kein Kettensuffix).
#[derive(Clone, PartialEq, Eq)]
pub struct CgroupRule {
    kernel_id: NonZeroU64,
    level: u32,
    chain_suffix: String,
}

impl fmt::Debug for CgroupRule {
    // Inhaltsfrei: Kernel-ID und Suffix (Hex der cgroup-Kennung) fehlen.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CgroupRule")
            .field("level", &self.level)
            .finish_non_exhaustive()
    }
}

impl CgroupRule {
    /// Prüft und baut eine Regel.
    ///
    /// # Arguments
    /// - `kernel_id` (`u64`): cgroup-ID der Ziel-cgroup (Inode ihres
    ///   cgroup2-Verzeichnisses).
    /// - `level` (`u32`): Tiefe der Ziel-cgroup unter der cgroup2-Wurzel.
    /// - `chain_suffix` (`&str`): Kleinbuchstaben-Hex-Stamm der Kettennamen.
    ///
    /// # Returns
    /// Die Regel.
    ///
    /// # Errors
    /// - [`NftError::InvalidKernelId`]: `kernel_id == 0`.
    /// - [`NftError::InvalidLevel`]: `level == 0` oder `level > 255`.
    /// - [`NftError::InvalidChainSuffix`]: leer, ungerade Länge, zu lang
    ///   oder nicht `0-9a-f`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_nft::{CgroupRule, NftError};
    ///
    /// assert!(CgroupRule::new(7, 1, "6a").is_ok());
    /// assert_eq!(CgroupRule::new(7, 256, "6a"), Err(NftError::InvalidLevel));
    /// ```
    pub fn new(kernel_id: u64, level: u32, chain_suffix: &str) -> NftResult<Self> {
        let kernel_id = NonZeroU64::new(kernel_id).ok_or(NftError::InvalidKernelId)?;
        if !(1..=MAX_SOCKET_LEVEL).contains(&level) {
            return Err(NftError::InvalidLevel);
        }
        validate_chain_suffix(chain_suffix)?;
        Ok(Self {
            kernel_id,
            level,
            chain_suffix: chain_suffix.to_owned(),
        })
    }

    /// Kernel-ID der Ziel-cgroup.
    ///
    /// # Returns
    /// Die ID (nie `0`).
    #[must_use]
    pub fn kernel_id(&self) -> u64 {
        self.kernel_id.get()
    }

    /// Tiefe der Ziel-cgroup unter der cgroup2-Wurzel.
    ///
    /// # Returns
    /// Die Ebene (`1..=255`).
    #[must_use]
    pub fn level(&self) -> u32 {
        self.level
    }

    /// Geprüfter Kettensuffix.
    ///
    /// # Returns
    /// Den Suffix (Kleinbuchstaben-Hex, gerade Länge).
    #[must_use]
    pub fn chain_suffix(&self) -> &str {
        &self.chain_suffix
    }

    /// Name der Basiskette am `output`-Hook.
    ///
    /// # Returns
    /// `o_<suffix>`.
    #[must_use]
    pub fn output_chain(&self) -> String {
        self.chain_name(Hook::Output)
    }

    /// Name der Basiskette am `input`-Hook.
    ///
    /// # Returns
    /// `i_<suffix>`.
    #[must_use]
    pub fn input_chain(&self) -> String {
        self.chain_name(Hook::Input)
    }

    /// Name der Basiskette an `hook`.
    pub(crate) fn chain_name(&self, hook: Hook) -> String {
        chain_name(hook, &self.chain_suffix)
    }
}

/// Name der Basiskette an `hook` für einen (geprüften) Suffix.
pub(crate) fn chain_name(hook: Hook, chain_suffix: &str) -> String {
    let mut name = String::with_capacity(CHAIN_PREFIX_LEN.saturating_add(chain_suffix.len()));
    name.push_str(hook.chain_prefix());
    name.push_str(chain_suffix);
    name
}

/// Prüft einen Kettensuffix (siehe Moduldoku, „Prüfregeln“); auch für
/// [`crate::NftNetlink::release_chains`], das ohne Kernel-ID auskommt.
pub(crate) fn validate_chain_suffix(suffix: &str) -> NftResult<()> {
    if suffix.is_empty() {
        return Err(NftError::InvalidChainSuffix { reason: "empty" });
    }
    if suffix.len() > MAX_CHAIN_SUFFIX_LEN {
        return Err(NftError::InvalidChainSuffix { reason: "too long" });
    }
    if !suffix.len().is_multiple_of(2) {
        return Err(NftError::InvalidChainSuffix {
            reason: "odd length",
        });
    }
    if !suffix
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(NftError::InvalidChainSuffix {
            reason: "not lowercase hex",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{CgroupRule, MAX_CHAIN_SUFFIX_LEN, MAX_SOCKET_LEVEL};
    use crate::error::NftError;
    use crate::test_support::TestResult;

    #[test]
    fn test_limits_follow_the_header_and_kernel_bounds() -> TestResult {
        // NFT_CHAIN_MAXNAMELEN = 256 (nf_tables.h:7) incl. NUL -> 255 chars;
        // "o_" + 252 = 254 fits, "o_" + 254 = 256 does not.
        assert_eq!(MAX_CHAIN_SUFFIX_LEN, 252);
        assert_eq!(MAX_SOCKET_LEVEL, 255);
        Ok(())
    }

    #[test]
    fn test_valid_rule_keeps_its_fields() -> TestResult {
        let rule = CgroupRule::new(u64::MAX, MAX_SOCKET_LEVEL, "00ff")?;
        assert_eq!(rule.kernel_id(), u64::MAX);
        assert_eq!(rule.level(), 255);
        assert_eq!(rule.chain_suffix(), "00ff");
        assert_eq!(rule.output_chain(), "o_00ff");
        assert_eq!(rule.input_chain(), "i_00ff");
        Ok(())
    }

    #[test]
    fn test_zero_kernel_id_is_rejected() -> TestResult {
        assert_eq!(CgroupRule::new(0, 1, "6a"), Err(NftError::InvalidKernelId));
        Ok(())
    }

    #[test]
    fn test_level_bounds() -> TestResult {
        for (level, ok) in [(0, false), (1, true), (2, true), (255, true), (256, false)] {
            let result = CgroupRule::new(7, level, "6a");
            assert_eq!(result.is_ok(), ok, "level {level}");
            if !ok {
                assert_eq!(result, Err(NftError::InvalidLevel));
            }
        }
        assert_eq!(
            CgroupRule::new(7, u32::MAX, "6a"),
            Err(NftError::InvalidLevel)
        );
        Ok(())
    }

    #[test]
    fn test_chain_suffix_rules() -> TestResult {
        let longest = "ab".repeat(MAX_CHAIN_SUFFIX_LEN / 2);
        assert!(CgroupRule::new(7, 1, &longest).is_ok());
        let too_long = "ab".repeat((MAX_CHAIN_SUFFIX_LEN / 2).saturating_add(1));
        let cases: [(&str, &'static str); 9] = [
            ("", "empty"),
            ("a", "odd length"),
            ("abc", "odd length"),
            ("AB", "not lowercase hex"),
            ("0g", "not lowercase hex"),
            ("a/", "not lowercase hex"),
            ("a\u{0}", "not lowercase hex"),
            ("\u{202e}a", "not lowercase hex"),
            (&too_long, "too long"),
        ];
        for (suffix, reason) in cases {
            assert_eq!(
                CgroupRule::new(7, 1, suffix),
                Err(NftError::InvalidChainSuffix { reason }),
                "suffix {suffix:?}"
            );
        }
        Ok(())
    }

    // Debug of a rule (what harw-warden might log with `?`) shows neither
    // the kernel id nor the chain suffix (hex of the cgroup id), in any
    // rendering.
    #[test]
    fn test_rule_debug_shows_neither_kernel_id_nor_suffix() -> TestResult {
        let rule = CgroupRule::new(424_242, 7, "6a6f622d31")?;
        for rendered in [format!("{rule:?}"), format!("{rule:#?}")] {
            assert!(rendered.contains("CgroupRule"), "{rendered}");
            assert!(rendered.contains("level"), "{rendered}");
            assert!(rendered.contains('7'), "{rendered}");
            assert!(!rendered.contains("424242"), "{rendered}");
            assert!(!rendered.contains("424_242"), "{rendered}");
            assert!(!rendered.contains("6a6f622d31"), "{rendered}");
            assert!(!rendered.contains("kernel_id"), "{rendered}");
            assert!(!rendered.contains("chain_suffix"), "{rendered}");
        }
        // The same holds inside a Result, as `assert_eq!` failures print it.
        let result: Result<CgroupRule, NftError> = Ok(rule);
        assert!(!format!("{result:?}").contains("6a6f622d31"));
        Ok(())
    }

    #[test]
    fn test_every_hex_digit_is_accepted() -> TestResult {
        let all = "0123456789abcdef";
        let rule = CgroupRule::new(1, 1, all)?;
        assert_eq!(rule.chain_suffix(), all);
        Ok(())
    }
}

//! Zuordnung von Peer-Identität zu [`PermissionTier`] — die Grundlage der
//! Tier-Ablehnungsmatrix.
//!
//! # Verantwortungsbereich
//! `harw-web` entscheidet nie selbst, welche Berechtigungsstufe ein Aufrufer
//! hat — das wäre ein zweiter Autoritätspfad. Stattdessen nimmt es eine von
//! außen (serverseitig, zur Komposition) übergebene Richtlinie entgegen, die
//! [`crate::peer::PeerCredentials`] auf eine
//! [`harw_operations::operation::PermissionTier`] abbildet — dasselbe
//! Kompositionsprinzip wie `harw_mcp_server::transport::PrincipalRegistry`
//! (dort: „Built once at composition time from server-trusted configuration;
//! never derived from an MCP request.").
//!
//! # Tier-Ablehnungsmatrix
//! [`tier_permits`] ist die eine Stelle, die entscheidet, ob ein Aufrufer
//! einer bestimmten Stufe eine Route einer bestimmten Mindeststufe erreichen
//! darf. Sie nutzt ausschließlich die bereits in `harw-operations`
//! definierte Totalordnung von [`PermissionTier`] — es gibt keine zweite,
//! web-eigene Rangfolge.
//!
//! # Nebenläufigkeit
//! [`PeerAuthorizer`]-Implementierungen müssen `Send + Sync` sein, damit sie
//! hinter `Arc` über Verbindungs-Tasks geteilt werden können.

use harw_operations::operation::PermissionTier;

use crate::peer::PeerCredentials;

/// Löst die Berechtigungsstufe eines identifizierten Peers auf.
///
/// # Description
/// Implementierungen werden bei der Komposition des Servers einmalig aus
/// vertrauenswürdiger, serverseitiger Konfiguration gebaut — nie aus einem
/// eingehenden HTTP-Request. `harw-web` ruft [`Self::tier_for`] pro
/// Verbindung genau einmal auf.
///
/// # Concurrency
/// Muss `Send + Sync` sein: `harw-web` hält Implementierungen hinter `Arc`
/// und teilt sie über alle Verbindungs-Tasks.
pub trait PeerAuthorizer: Send + Sync {
    /// Liefert die Berechtigungsstufe für `peer`, falls bekannt.
    ///
    /// # Arguments
    /// - `peer` (`&PeerCredentials`): die über `SO_PEERCRED` gelesene Identität.
    ///
    /// # Returns
    /// - `Some(tier)`: der Peer ist einer Stufe zugeordnet.
    /// - `None`: der Peer ist unbekannt; `harw-web` muss die Verbindung als
    ///   nicht autorisiert behandeln (kein impliziter Rückfall auf
    ///   [`PermissionTier::Observer`] — ein unbekannter Peer ist etwas
    ///   anderes als ein bekannter Peer der niedrigsten Stufe).
    fn tier_for(&self, peer: &PeerCredentials) -> Option<PermissionTier>;
}

/// Statische, UID-basierte Autorisierung — die einfachste Implementierung
/// von [`PeerAuthorizer`].
///
/// # Description
/// Bildet effektive Nutzer-IDs auf [`PermissionTier`] ab. Optional trägt sie
/// einen `default`-Wert für jede UID, die nicht explizit gelistet ist — ohne
/// `default` bleibt ein unbekannter Peer unautorisiert
/// ([`PeerAuthorizer::tier_for`] liefert `None`).
///
/// # Beispiel
/// ```rust
/// use harw_operations::operation::PermissionTier;
/// use harw_web::authz::{PeerAuthorizer, StaticUidTierMap};
/// use harw_web::peer::PeerCredentials;
///
/// let authz = StaticUidTierMap::new(vec![(1000, PermissionTier::Owner)]);
/// assert_eq!(
///     authz.tier_for(&PeerCredentials::new(1, 1000, 1000)),
///     Some(PermissionTier::Owner)
/// );
/// assert_eq!(authz.tier_for(&PeerCredentials::new(2, 9999, 9999)), None);
/// ```
pub struct StaticUidTierMap {
    entries: Vec<(u32, PermissionTier)>,
    default: Option<PermissionTier>,
}

impl StaticUidTierMap {
    /// Baut eine Zuordnung ohne Rückfallwert — eine nicht gelistete UID
    /// bleibt unautorisiert.
    ///
    /// # Arguments
    /// - `entries` (`Vec<(u32, PermissionTier)>`): UID-zu-Tier-Paare.
    ///
    /// # Returns
    /// Eine neue `StaticUidTierMap`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_operations::operation::PermissionTier;
    /// use harw_web::authz::StaticUidTierMap;
    ///
    /// let _authz = StaticUidTierMap::new(vec![(0, PermissionTier::Owner)]);
    /// ```
    #[must_use]
    pub fn new(entries: Vec<(u32, PermissionTier)>) -> Self {
        Self {
            entries,
            default: None,
        }
    }

    /// Baut eine Zuordnung mit Rückfallwert für nicht gelistete UIDs.
    ///
    /// # Arguments
    /// - `entries` (`Vec<(u32, PermissionTier)>`): UID-zu-Tier-Paare.
    /// - `default` (`PermissionTier`): Stufe für jede nicht gelistete UID.
    ///
    /// # Returns
    /// Eine neue `StaticUidTierMap`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_operations::operation::PermissionTier;
    /// use harw_web::authz::StaticUidTierMap;
    ///
    /// let _authz = StaticUidTierMap::with_default(vec![], PermissionTier::Observer);
    /// ```
    #[must_use]
    pub fn with_default(entries: Vec<(u32, PermissionTier)>, default: PermissionTier) -> Self {
        Self {
            entries,
            default: Some(default),
        }
    }
}

impl PeerAuthorizer for StaticUidTierMap {
    fn tier_for(&self, peer: &PeerCredentials) -> Option<PermissionTier> {
        self.entries
            .iter()
            .find(|(uid, _)| *uid == peer.uid)
            .map(|(_, tier)| *tier)
            .or(self.default)
    }
}

/// Entscheidet, ob eine Aufruferstufe eine Route einer Mindeststufe erreichen darf.
///
/// # Description
/// Die eine Funktion, die die Tier-Ablehnungsmatrix trägt: nutzt
/// ausschließlich die in `harw-operations` definierte Totalordnung von
/// [`PermissionTier`] (`Observer < Operator < Maintainer < Owner`).
///
/// # Arguments
/// - `caller` (`PermissionTier`): die aufgelöste Stufe des Aufrufers.
/// - `required` (`PermissionTier`): die von der Route verlangte Mindeststufe
///   (`WebAdapter::permission()`).
///
/// # Returns
/// `true`, wenn `caller >= required`; sonst `false`.
///
/// # Examples
/// ```rust
/// use harw_operations::operation::PermissionTier;
/// use harw_web::authz::tier_permits;
///
/// assert!(tier_permits(PermissionTier::Owner, PermissionTier::Observer));
/// assert!(!tier_permits(PermissionTier::Observer, PermissionTier::Owner));
/// ```
#[must_use]
pub fn tier_permits(caller: PermissionTier, required: PermissionTier) -> bool {
    caller >= required
}

#[cfg(test)]
mod tests {
    use super::{PeerAuthorizer, StaticUidTierMap, tier_permits};
    use crate::peer::PeerCredentials;
    use harw_operations::operation::PermissionTier;

    fn peer(uid: u32) -> PeerCredentials {
        PeerCredentials::new(1, uid, uid)
    }

    #[test]
    fn test_static_uid_tier_map_resolves_listed_uid() {
        let authz = StaticUidTierMap::new(vec![(1000, PermissionTier::Maintainer)]);
        assert_eq!(authz.tier_for(&peer(1000)), Some(PermissionTier::Maintainer));
    }

    #[test]
    fn test_static_uid_tier_map_unknown_uid_without_default_is_none() {
        let authz = StaticUidTierMap::new(vec![(1000, PermissionTier::Maintainer)]);
        assert_eq!(authz.tier_for(&peer(9999)), None);
    }

    #[test]
    fn test_static_uid_tier_map_unknown_uid_with_default_falls_back() {
        let authz =
            StaticUidTierMap::with_default(vec![(1000, PermissionTier::Owner)], PermissionTier::Observer);
        assert_eq!(authz.tier_for(&peer(9999)), Some(PermissionTier::Observer));
        assert_eq!(authz.tier_for(&peer(1000)), Some(PermissionTier::Owner));
    }

    #[test]
    fn test_static_uid_tier_map_empty_entries_uses_default_for_everyone() {
        let authz = StaticUidTierMap::with_default(vec![], PermissionTier::Operator);
        assert_eq!(authz.tier_for(&peer(1)), Some(PermissionTier::Operator));
        assert_eq!(authz.tier_for(&peer(2)), Some(PermissionTier::Operator));
    }

    // ── tier_permits — je Tier ein erlaubter und ein abgelehnter Fall ─────────

    #[test]
    fn test_tier_permits_observer_caller_allowed_for_observer_route() {
        assert!(tier_permits(PermissionTier::Observer, PermissionTier::Observer));
    }

    #[test]
    fn test_tier_permits_observer_caller_denied_for_operator_route() {
        assert!(!tier_permits(PermissionTier::Observer, PermissionTier::Operator));
    }

    #[test]
    fn test_tier_permits_operator_caller_allowed_for_operator_route() {
        assert!(tier_permits(PermissionTier::Operator, PermissionTier::Operator));
    }

    #[test]
    fn test_tier_permits_operator_caller_denied_for_maintainer_route() {
        assert!(!tier_permits(PermissionTier::Operator, PermissionTier::Maintainer));
    }

    #[test]
    fn test_tier_permits_maintainer_caller_allowed_for_maintainer_route() {
        assert!(tier_permits(PermissionTier::Maintainer, PermissionTier::Maintainer));
    }

    #[test]
    fn test_tier_permits_maintainer_caller_denied_for_owner_route() {
        assert!(!tier_permits(PermissionTier::Maintainer, PermissionTier::Owner));
    }

    #[test]
    fn test_tier_permits_owner_caller_allowed_for_owner_route() {
        assert!(tier_permits(PermissionTier::Owner, PermissionTier::Owner));
    }

    #[test]
    fn test_tier_permits_owner_caller_allowed_for_observer_route_too() {
        // Owner ist die höchste Stufe; sie darf jede niedrigere Route erreichen —
        // das ist kein abgelehnter Fall, sondern belegt die Totalordnung nach oben.
        assert!(tier_permits(PermissionTier::Owner, PermissionTier::Observer));
    }
}

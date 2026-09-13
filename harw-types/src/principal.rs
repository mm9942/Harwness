//! Vertrauenswürdige Identität eines Aufrufers (`Principal`) und die geordnete
//! Berechtigungsstufe [`PermissionTier`].
//!
//! # Beschreibung
//! Ein [`Principal`] beschreibt, *wer* über *welche Eingangsfläche* mit *welcher
//! Stufe* handelt. Er entsteht ausschließlich an einer vertrauenswürdigen
//! Eingangsgrenze ([`Principal::trusted_ingress`]) oder durch Absenkung aus
//! einem Eltern-Principal ([`Principal::child_of`]). Bewusst gibt es **kein**
//! `Deserialize`: ein Principal darf nie aus Wire-, Konfigurations- oder
//! Modelldaten rekonstruiert werden.
//!
//! [`PermissionTier`] lebt seit Welle W0b hier (vorher
//! `harw-operations/src/operation.rs`); `harw-operations` re-exportiert den Typ
//! unverändert als `harw_operations::operation::PermissionTier`.

use serde::{Deserialize, Serialize};

use crate::ids::ApprovalActor;

/// Geordnete Mindest-Berechtigungsstufe für eine Operation.
///
/// # Beschreibung
/// Die Varianten sind totalgeordnet (`PartialOrd + Ord`), von niedrigster
/// (`Observer`) bis höchster (`Owner`) Berechtigungsstufe. Ein Aufrufer
/// mit Stufe `X` darf alle Operationen ausführen, deren `permission <= X`.
///
/// Die Definition liegt im abhängigkeitsfreien `harw-types`, damit sowohl
/// `harw-operations` als auch [`Principal`] denselben Typ verwenden, ohne
/// dass `harw-types` von einem höherstufigen Crate abhängt.
///
/// # Serialisierung
/// `snake_case`: `"observer"`, `"operator"`, `"maintainer"`, `"owner"`.
///
/// # Beispiel
/// ```rust
/// use harw_types::PermissionTier;
///
/// assert!(PermissionTier::Maintainer > PermissionTier::Operator);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionTier {
    /// Lesezugriff; keine zustandsverändernden Aktionen.
    Observer,
    /// Standardnutzer; darf Operationen im normalen Betrieb ausführen.
    Operator,
    /// Erweiterte Rechte; darf Konfiguration und Ressourcen verwalten.
    Maintainer,
    /// Vollzugriff; darf alle Operationen einschließlich destruktiver Aktionen ausführen.
    Owner,
}

/// Art des handelnden Subjekts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    /// Ein Mensch an einer lokalen oder authentifizierten Oberfläche.
    Human,
    /// Ein Modell (Root-Agent oder abgeleiteter Kind-Agent).
    Model,
    /// Eine systemseitig ausgelöste Operation (Job, Plan-Knoten, Wartung).
    Operation,
    /// Ein externer Kanal (MCP-Client, Telegram-Peer, Gateway).
    Channel,
}

/// Eingangsfläche, über die ein [`Principal`] den Harness erreicht.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngressSurface {
    /// Interaktive Terminal-Oberfläche.
    Tui,
    /// Nicht-interaktive Kommandozeile.
    Cli,
    /// Lokale Web-Oberfläche (Unix-Socket).
    Web,
    /// MCP-Server-Transport.
    Mcp,
    /// Telegram-Kanal.
    Telegram,
    /// Gateway-Prozess (z. B. Dream-Läufe).
    Gateway,
    /// Durabler Job-Worker.
    JobWorker,
    /// Von einem Eltern-Principal abgeleiteter Kind-Agent.
    Child,
}

/// Vertrauenswürdige Identität eines Aufrufers.
///
/// # Beschreibung
/// Alle Felder sind privat; der einzige Weg zu einem `Principal` führt über
/// [`Self::trusted_ingress`] (Eingangsgrenze) oder [`Self::child_of`]
/// (monotone Absenkung). Bewusst **kein** `Deserialize`.
///
/// # Beispiel
/// ```rust
/// use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind};
///
/// let root = Principal::trusted_ingress(
///     PrincipalKind::Human,
///     "mia",
///     IngressSurface::Tui,
///     PermissionTier::Owner,
/// );
/// let child = root.child_of("explorer");
/// assert_eq!(child.tier(), PermissionTier::Operator);
/// assert_eq!(child.surface(), IngressSurface::Child);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Principal {
    kind: PrincipalKind,
    id: String,
    surface: IngressSurface,
    tier: PermissionTier,
}

impl Principal {
    /// Baut einen Principal an einer vertrauenswürdigen Eingangsgrenze.
    ///
    /// # Parameter
    /// - `kind`: Art des Subjekts.
    /// - `id`: stabile, von der Eingangsgrenze authentifizierte Kennung —
    ///   niemals ein vom Modell oder aus Chat-Text geliefertes Etikett.
    /// - `surface`: Eingangsfläche.
    /// - `tier`: von der Eingangsgrenze zugeteilte Stufe.
    #[must_use]
    pub fn trusted_ingress(
        kind: PrincipalKind,
        id: impl Into<String>,
        surface: IngressSurface,
        tier: PermissionTier,
    ) -> Self {
        Self {
            kind,
            id: id.into(),
            surface,
            tier,
        }
    }

    /// Art des Subjekts.
    #[must_use]
    pub fn kind(&self) -> PrincipalKind {
        self.kind
    }

    /// Authentifizierte Kennung.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Eingangsfläche.
    #[must_use]
    pub fn surface(&self) -> IngressSurface {
        self.surface
    }

    /// Zugeteilte Berechtigungsstufe.
    #[must_use]
    pub fn tier(&self) -> PermissionTier {
        self.tier
    }

    /// Leitet einen Kind-Principal für einen gespawnten Agenten ab.
    ///
    /// # Beschreibung
    /// Das Kind ist immer [`PrincipalKind::Model`] auf
    /// [`IngressSurface::Child`]; seine Stufe ist
    /// `min(eltern.tier, PermissionTier::Operator)` — sie kann also nur
    /// sinken, nie steigen. Die Kennung lautet `"<eltern-id>/<role>"`, damit
    /// die Herkunft im Audit nachvollziehbar bleibt.
    #[must_use]
    pub fn child_of(&self, role: &str) -> Principal {
        Principal {
            kind: PrincipalKind::Model,
            id: format!("{}/{}", self.id, role),
            surface: IngressSurface::Child,
            tier: self.tier.min(PermissionTier::Operator),
        }
    }

    /// Bildet den Principal auf den Freigabe-Akteur ab, der Approval-Prompts
    /// beantworten darf.
    ///
    /// # Abbildung
    /// | Kind × Surface | Ergebnis |
    /// |---|---|
    /// | `Human` × `Tui` | `ApprovalActor::Operator { id: "local-tui" }` |
    /// | `Human` × `Cli` | `ApprovalActor::Operator { id: "local-cli" }` |
    /// | `Human` × `Web` | `ApprovalActor::Operator { id: "owner" }` |
    /// | `Channel` × `Mcp` | `ApprovalActor::Operator { id: <self.id> }` |
    /// | alle übrigen | `None` |
    ///
    /// `ApprovalActor::ChannelPeer` wird hier bewusst nie erzeugt: er
    /// verlangt eine `ChannelId` + `PeerId`-Bindung, die ein Principal nicht
    /// trägt; Kanäle mit Peer-Bindung (Telegram) liefern ihren Akteur selbst.
    #[must_use]
    pub fn approval_actor(&self) -> Option<ApprovalActor> {
        let id = match (self.kind, self.surface) {
            (PrincipalKind::Human, IngressSurface::Tui) => "local-tui".to_owned(),
            (PrincipalKind::Human, IngressSurface::Cli) => "local-cli".to_owned(),
            (PrincipalKind::Human, IngressSurface::Web) => "owner".to_owned(),
            (PrincipalKind::Channel, IngressSurface::Mcp) => self.id.clone(),
            _ => return None,
        };
        Some(ApprovalActor::Operator { id })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINDS: [PrincipalKind; 4] = [
        PrincipalKind::Human,
        PrincipalKind::Model,
        PrincipalKind::Operation,
        PrincipalKind::Channel,
    ];

    const SURFACES: [IngressSurface; 8] = [
        IngressSurface::Tui,
        IngressSurface::Cli,
        IngressSurface::Web,
        IngressSurface::Mcp,
        IngressSurface::Telegram,
        IngressSurface::Gateway,
        IngressSurface::JobWorker,
        IngressSurface::Child,
    ];

    const TIERS: [PermissionTier; 4] = [
        PermissionTier::Observer,
        PermissionTier::Operator,
        PermissionTier::Maintainer,
        PermissionTier::Owner,
    ];

    fn operator(id: &str) -> Option<ApprovalActor> {
        Some(ApprovalActor::Operator { id: id.to_owned() })
    }

    #[test]
    fn approval_actor_table_covers_every_kind_and_surface() {
        for kind in KINDS {
            for surface in SURFACES {
                let principal = Principal::trusted_ingress(
                    kind,
                    "client-7",
                    surface,
                    PermissionTier::Owner,
                );
                let expected = match (kind, surface) {
                    (PrincipalKind::Human, IngressSurface::Tui) => operator("local-tui"),
                    (PrincipalKind::Human, IngressSurface::Cli) => operator("local-cli"),
                    (PrincipalKind::Human, IngressSurface::Web) => operator("owner"),
                    (PrincipalKind::Channel, IngressSurface::Mcp) => operator("client-7"),
                    _ => None,
                };
                assert_eq!(
                    principal.approval_actor(),
                    expected,
                    "kind={kind:?} surface={surface:?}"
                );
            }
        }
    }

    #[test]
    fn approval_actor_count_of_some_is_exactly_four() {
        let mut some = 0;
        for kind in KINDS {
            for surface in SURFACES {
                let tier = PermissionTier::Observer;
                let p = Principal::trusted_ingress(kind, "x", surface, tier);
                if p.approval_actor().is_some() {
                    some += 1;
                }
            }
        }
        assert_eq!(some, 4);
    }

    #[test]
    fn child_of_lowers_tier_to_at_most_operator() {
        let expected = [
            (PermissionTier::Observer, PermissionTier::Observer),
            (PermissionTier::Operator, PermissionTier::Operator),
            (PermissionTier::Maintainer, PermissionTier::Operator),
            (PermissionTier::Owner, PermissionTier::Operator),
        ];
        for (parent_tier, child_tier) in expected {
            let parent = Principal::trusted_ingress(
                PrincipalKind::Human,
                "mia",
                IngressSurface::Tui,
                parent_tier,
            );
            let child = parent.child_of("explorer");
            assert_eq!(child.tier(), child_tier, "parent={parent_tier:?}");
            assert!(child.tier() <= parent.tier());
        }
    }

    #[test]
    fn child_of_sets_model_kind_child_surface_and_derived_id() {
        for kind in KINDS {
            for surface in SURFACES {
                for tier in TIERS {
                    let parent = Principal::trusted_ingress(kind, "root", surface, tier);
                    let child = parent.child_of("reviewer");
                    assert_eq!(child.kind(), PrincipalKind::Model);
                    assert_eq!(child.surface(), IngressSurface::Child);
                    assert_eq!(child.id(), "root/reviewer");
                    // Ein Kind ist nie selbst ein Freigabe-Akteur.
                    assert_eq!(child.approval_actor(), None);
                }
            }
        }
    }

    #[test]
    fn child_of_is_monotone_across_generations() {
        let root = Principal::trusted_ingress(
            PrincipalKind::Human,
            "mia",
            IngressSurface::Cli,
            PermissionTier::Owner,
        );
        let grandchild = root.child_of("a").child_of("b");
        assert_eq!(grandchild.tier(), PermissionTier::Operator);
        assert_eq!(grandchild.id(), "mia/a/b");
    }

    #[test]
    fn accessors_return_ingress_values() {
        let p = Principal::trusted_ingress(
            PrincipalKind::Operation,
            String::from("job-1"),
            IngressSurface::JobWorker,
            PermissionTier::Maintainer,
        );
        assert_eq!(p.kind(), PrincipalKind::Operation);
        assert_eq!(p.id(), "job-1");
        assert_eq!(p.surface(), IngressSurface::JobWorker);
        assert_eq!(p.tier(), PermissionTier::Maintainer);
    }

    #[test]
    fn permission_tier_is_totally_ordered() {
        assert!(PermissionTier::Observer < PermissionTier::Operator);
        assert!(PermissionTier::Operator < PermissionTier::Maintainer);
        assert!(PermissionTier::Maintainer < PermissionTier::Owner);
        let mut shuffled = [
            PermissionTier::Owner,
            PermissionTier::Observer,
            PermissionTier::Maintainer,
            PermissionTier::Operator,
        ];
        shuffled.sort();
        assert_eq!(shuffled, TIERS);
    }

    #[test]
    fn permission_tier_serde_uses_snake_case_names() {
        let names = ["observer", "operator", "maintainer", "owner"];
        for (tier, name) in TIERS.iter().zip(names) {
            let json = serde_json::to_string(tier).unwrap();
            assert_eq!(json, format!("\"{name}\""));
            let back: PermissionTier = serde_json::from_str(&json).unwrap();
            assert_eq!(back, *tier);
        }
        assert!(serde_json::from_str::<PermissionTier>("\"Owner\"").is_err());
    }

    #[test]
    fn principal_serializes_all_fields_snake_case() {
        let p = Principal::trusted_ingress(
            PrincipalKind::Channel,
            "peer",
            IngressSurface::JobWorker,
            PermissionTier::Observer,
        );
        let value = serde_json::to_value(&p).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "kind": "channel",
                "id": "peer",
                "surface": "job_worker",
                "tier": "observer",
            })
        );
    }
}

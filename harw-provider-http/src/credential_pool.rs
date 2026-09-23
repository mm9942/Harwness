//! Auswahl + gebundenes Failover für `[[credential_pool.<provider>]]` aus
//! `auth.toml` (`harw_config::AuthConfig::credential_pool`).
//!
//! ## Verantwortung
//! Reale Nutzer hinterlegen für einen Provider oft mehrere Credentials
//! (z. B. drei `openai`-Keys mit getrennten Kontingenten). Dieses Modul
//! besitzt die Laufzeit-Auswahl **eines** aktiven Eintrags in
//! Prioritätsreihenfolge sowie das gebundene Failover, wenn dieser Eintrag
//! ausfällt — nicht das Parsen der TOML-Struktur selbst (das bleibt
//! `harw_config::auth_toml::CredentialEntry`) und nicht den eigentlichen
//! HTTP-Request (das bleibt Sache von [`crate::OpenAiResponsesProvider`] und
//! [`crate::anthropic::AnthropicMessagesProvider`]).
//!
//! ## Präzedenzregel (überarbeitet: primäres Credential zuerst)
//! `provider.auth` (die einzelne `SecretRef` in `providers/<name>.toml`) —
//! bei Anthropic zusätzlich die implizite Umgebungs-Auflösung über
//! `resolve_anthropic`, wenn `provider.auth` fehlt — ist **immer** das
//! zuerst versuchte Credential. Ein konfigurierter `credential_pool`
//! liefert ausschließlich **Failover-Kandidaten**: sie werden erst
//! probiert, nachdem das primäre Credential mit einem Auth- oder
//! Kontingent-Fehler fehlschlägt (siehe [`should_failover`]), und dann in
//! aufsteigender `priority`-Reihenfolge (stabil nach Datei-Reihenfolge).
//! Fehlt `provider.auth` (und liefert bei Anthropic auch die implizite
//! Auflösung keinen Treffer), wird stattdessen der erste Pool-Eintrag zum
//! primären Credential; die übrigen Pool-Einträge bleiben Failover-
//! Kandidaten dahinter. Ist der Pool leer oder für den Provider gar nicht
//! vorhanden, ändert sich nichts: nur `provider.auth` (bzw. die implizite
//! Auflösung) wird verwendet. Diese Regel gilt *pro Provider*, nicht
//! global — ein Provider ohne Pool ist von einem Pool eines anderen
//! Providers nicht betroffen.
//!
//! Ein `base_url`-Override eines Pool-Eintrags (`CredentialEntry::base_url`)
//! gilt nur, während genau dieser Eintrag aktiv ist — das primäre Credential
//! nutzt stets `provider.base_url` (bzw. bei Anthropic die von
//! `resolve_anthropic` gelieferte Basis-URL), niemals einen Pool-Override.
//!
//! Intern wird das primäre Credential (falls vorhanden) über
//! [`CredentialPool::prepend_primary`] als Index `0` vor die aus
//! `auth.toml` gelesenen Pool-Einträge gestellt. [`CredentialPool::select`]
//! bevorzugt ohnehin immer den niedrigsten nicht abkühlenden Index — das
//! primäre Credential wird also automatisch zuerst versucht, ohne dass
//! `select`/`mark_cooldown`/`should_failover` selbst angepasst werden
//! mussten.
//!
//! ## Auswahl + Failover
//! - Reihenfolge: primäres Credential (Index `0`, falls vorhanden), dann
//!   die Pool-Einträge aus [`harw_config::AuthConfig::credential_pool_ordered`]
//!   (aufsteigend nach `priority`, stabil nach Datei-Reihenfolge).
//! - Ein Eintrag, der gerade abgekühlt wird (`cooldown_until` in der
//!   Zukunft), wird bei der Auswahl übersprungen, außer **alle** Einträge
//!   kühlen gerade ab — dann wird trotzdem einer versucht (fail-open: eine
//!   Anfrage darf nie ins Leere laufen, nur weil jeder Eintrag kürzlich
//!   einmal fehlschlug).
//! - [`should_failover`] entscheidet, welche Fehlerklasse einen
//!   Credential-Wechsel rechtfertigt: nur [`ModelError::Auth`] (401/403 –
//!   das Credential selbst wurde abgelehnt) und [`ModelError::QuotaExceeded`]
//!   (Kontingent dieses Credentials ist aufgebraucht). Rein transiente
//!   Fehler (`Transient`, `Timeout`, generisches `RateLimited`) bleiben
//!   bewusst außen vor — die erholen sich in aller Regel auf **demselben**
//!   Credential und werden bereits von [`crate::retry::RetryingProvider`]
//!   mit Backoff wiederholt; ein Credential-Wechsel würde dort nur unnötig
//!   ein zweites Kontingent belasten.
//! - Ein Fehlversuch markiert seinen Eintrag für [`DEFAULT_COOLDOWN`]
//!   (60 s, **bounded**, keine Sperre auf Dauer) und der Aufrufer wiederholt
//!   die **gleiche** Anfrage **genau einmal** mit dem nächsten nicht
//!   abkühlenden Eintrag — niemals mehr; schlägt auch dieser Versuch fehl,
//!   wird der zweite Fehler zurückgegeben (siehe `respond()` in
//!   `lib.rs`/`anthropic.rs`).
//!
//! ## Sicherheit
//! Log-Zeilen dieses Moduls und seiner Aufrufer nennen ausschließlich
//! [`PoolEntry::label`] (freier Text aus `auth.toml`, oder ein synthetisches
//! `"<provider>#<index>"`) oder den Index — niemals den aufgelösten
//! Secret-Wert.
//!
//! ## Nebenläufigkeit
//! [`CredentialPool`] ist `Send + Sync`, wenn `T` es ist. Der
//! Cooldown-Zustand liegt in einem `std::sync::Mutex`, der nur für kurze,
//! synchrone Vergleiche gehalten wird — nie über ein `.await` hinweg.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use harw_config::AuthConfig;
use harw_core::ModelError;
use secrecy::SecretString;

use crate::HttpProviderResult;

/// Bounded Abkühlzeit eines Pool-Eintrags nach einem Auth-/Kontingent-Fehler.
///
/// Bewusst kein "für immer" — ein Kontingent kann sich erholen (z. B.
/// Tages-/Minutenlimit), und ein dauerhaft gesperrter Eintrag würde einen
/// Nutzer, der nur drei Credentials besitzt, nach drei Fehlversuchen für
/// immer aussperren.
pub(crate) const DEFAULT_COOLDOWN: Duration = Duration::from_secs(60);

/// Ein aufgelöster Pool-Eintrag: die vom Aufrufer gewählte
/// Credential-Repräsentation `T` (roher `SecretString`-Bearer-Token bei
/// [`crate::OpenAiResponsesProvider`], ein bereits klassifiziertes
/// [`crate::anthropic::AnthropicCredential`] bei
/// [`crate::anthropic::AnthropicMessagesProvider`]) plus die
/// selektionsrelevanten Metadaten aus `auth.toml`.
pub(crate) struct PoolEntry<T> {
    /// Die Credential-Repräsentation, die der Aufrufer für den Request braucht.
    pub(crate) value: T,
    /// Pool-lokale Basis-URL-Override (`CredentialEntry::base_url`); `None`
    /// heißt „Provider-Default verwenden".
    pub(crate) base_url: Option<String>,
    /// Nur zum Loggen: `CredentialEntry::label`, sonst `"<provider>#<index>"`.
    /// Niemals der Secret-Wert.
    pub(crate) label: String,
}

/// Geordneter, laufzeit-auswählbarer Pool von Credentials eines Providers.
///
/// Siehe Moduldoku für Präzedenz, Auswahl und Failover.
pub(crate) struct CredentialPool<T> {
    entries: Vec<PoolEntry<T>>,
    cooldown_until: Mutex<Vec<Option<Instant>>>,
    cooldown: Duration,
}

impl<T> CredentialPool<T> {
    /// Löst `auth.credential_pool[provider_name]` auf und baut daraus einen
    /// [`CredentialPool`].
    ///
    /// # Description
    /// Iteriert [`AuthConfig::credential_pool_ordered`] (Auswahlreihenfolge
    /// bereits aufsteigend nach `priority` sortiert) und löst jede
    /// `CredentialEntry::secret`-`SecretRef` über [`crate::resolve_secret`]
    /// auf. `build` übersetzt den aufgelösten [`SecretString`] in die vom
    /// Aufrufer benötigte Repräsentation `T` — z. B. identisch für
    /// OpenAI-kompatible Bearer-Tokens, oder klassifiziert nach
    /// Auth-Schema für [`crate::anthropic::AnthropicCredential`].
    ///
    /// # Arguments
    /// - `auth` (`&AuthConfig`): die geladene `auth.toml`.
    /// - `provider_name` (`&str`): Provider, dessen Pool aufgelöst wird.
    /// - `sources` ([`crate::SecretSources`]): Env-Layer/Resolver/Home für
    ///   [`crate::resolve_secret`], wie beim Auflösen von `provider.auth`.
    /// - `build` (`impl Fn(SecretString) -> HttpProviderResult<T>`):
    ///   Übersetzung Secret → Aufrufer-Repräsentation.
    ///
    /// # Returns
    /// `Ok(None)`, wenn für `provider_name` kein Pool konfiguriert ist (der
    /// Aufrufer fällt dann auf `provider.auth` zurück, siehe Moduldoku).
    /// Sonst `Ok(Some(pool))` mit mindestens einem Eintrag.
    ///
    /// # Errors
    /// Jeder Fehler, den [`crate::resolve_secret`] oder `build` für einen
    /// der Einträge liefert (die Auflösung bricht beim ersten Fehler ab).
    pub(crate) fn from_auth_config(
        auth: &AuthConfig,
        provider_name: &str,
        sources: crate::SecretSources<'_>,
        build: impl Fn(SecretString) -> HttpProviderResult<T>,
    ) -> HttpProviderResult<Option<Self>> {
        let ordered = auth.credential_pool_ordered(provider_name);
        if ordered.is_empty() {
            return Ok(None);
        }
        let mut entries = Vec::with_capacity(ordered.len());
        for (index, entry) in ordered.into_iter().enumerate() {
            let secret = crate::resolve_secret(&entry.secret, sources)?;
            let value = build(secret)?;
            let label = entry
                .label
                .clone()
                .unwrap_or_else(|| format!("{provider_name}#{index}"));
            entries.push(PoolEntry {
                value,
                base_url: entry.base_url.clone(),
                label,
            });
        }
        let entry_count = entries.len();
        Ok(Some(Self {
            entries,
            cooldown_until: Mutex::new(vec![None; entry_count]),
            cooldown: DEFAULT_COOLDOWN,
        }))
    }

    /// Anzahl der Einträge (Aufrufer nutzen dies, um Failover nur bei
    /// mindestens zwei Einträgen überhaupt zu versuchen).
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Stellt ein primäres Credential (`provider.auth` bzw. dessen implizite
    /// Auflösung) als Index `0` vor die bestehenden Pool-Einträge.
    ///
    /// # Description
    /// Setzt die in der Moduldoku beschriebene Präzedenz um: das primäre
    /// Credential wird von [`Self::select`] immer zuerst versucht (der
    /// niedrigste nicht abkühlende Index gewinnt), die bisherigen Einträge
    /// dieses Pools rücken auf Index `1..` und bleiben ausschließlich
    /// Failover-Kandidaten in ihrer bisherigen Relativreihenfolge. Der
    /// Cooldown-Zustand wird dabei für **alle** Einträge zurückgesetzt (ein
    /// frischer `Mutex`), da sich die Indizes verschieben — unproblematisch,
    /// weil `prepend_primary` ausschließlich einmalig bei der
    /// Provider-Konstruktion aufgerufen wird, nie zur Laufzeit auf einem
    /// bereits genutzten Pool.
    ///
    /// # Arguments
    /// - `value` (`T`): das primäre Credential.
    /// - `label` (`String`): Log-Label für Index `0`, üblicherweise
    ///   `"<provider>#primary"`.
    ///
    /// # Returns
    /// Einen neuen [`CredentialPool`] mit `value` an Index `0` (ohne
    /// `base_url`-Override — das primäre Credential nutzt immer
    /// `provider.base_url`) und den bisherigen Einträgen dahinter.
    #[must_use]
    pub(crate) fn prepend_primary(self, value: T, label: String) -> Self {
        let mut entries = Vec::with_capacity(self.entries.len() + 1);
        entries.push(PoolEntry {
            value,
            base_url: None,
            label,
        });
        entries.extend(self.entries);
        let entry_count = entries.len();
        Self {
            entries,
            cooldown_until: Mutex::new(vec![None; entry_count]),
            cooldown: self.cooldown,
        }
    }

    /// Liefert den Eintrag an `index`.
    ///
    /// # Panics
    /// Wenn `index` außerhalb des Pools liegt. Aufrufer verwenden
    /// ausschließlich Indizes, die zuvor von [`Self::select`] geliefert
    /// wurden, daher ist dies innerhalb dieses Moduls kein realer Pfad.
    pub(crate) fn entry(&self, index: usize) -> &PoolEntry<T> {
        &self.entries[index]
    }

    /// Wählt den bevorzugten, gerade nicht abkühlenden Eintrag außer
    /// `exclude`.
    ///
    /// # Description
    /// Läuft die Einträge in Auswahlreihenfolge (Index 0 = höchste
    /// Priorität) durch und liefert den ersten, dessen Cooldown bereits
    /// abgelaufen ist. Kühlen alle passenden Einträge gerade ab, wird
    /// trotzdem einer geliefert (fail-open, siehe Moduldoku) — sonst könnte
    /// ein Nutzer mit nur zwei Credentials nach zwei Fehlversuchen für die
    /// gesamte Cooldown-Dauer komplett blockiert sein.
    ///
    /// # Arguments
    /// - `exclude` (`Option<usize>`): Index, der nie zurückgegeben wird
    ///   (der zuletzt verwendete Eintrag beim Failover-Retry).
    ///
    /// # Returns
    /// `None` nur, wenn der Pool leer ist oder `exclude` der einzige Eintrag
    /// war.
    pub(crate) fn select(&self, exclude: Option<usize>) -> Option<usize> {
        let now = Instant::now();
        let cooldowns = self
            .cooldown_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (index, until) in cooldowns.iter().enumerate() {
            if Some(index) == exclude {
                continue;
            }
            let cooled_down = until.is_none_or(|until| until <= now);
            if cooled_down {
                return Some(index);
            }
        }
        // Alle verbleibenden Kandidaten kühlen ab: fail-open auf den ersten
        // Index != exclude, statt gar keinen Versuch mehr zuzulassen.
        (0..self.entries.len()).find(|&index| Some(index) != exclude)
    }

    /// Markiert `index` für [`DEFAULT_COOLDOWN`] als vorübergehend
    /// ausgesetzt. Kein Effekt bei ungültigem `index`.
    pub(crate) fn mark_cooldown(&self, index: usize) {
        let mut cooldowns = self
            .cooldown_until
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(slot) = cooldowns.get_mut(index) {
            *slot = Some(Instant::now() + self.cooldown);
        }
    }
}

/// Entscheidet, ob `error` einen Credential-Wechsel rechtfertigt.
///
/// # Description
/// Nur [`ModelError::Auth`] (401/403 — das Credential selbst ist ungültig
/// oder wurde entzogen) und [`ModelError::QuotaExceeded`] (das Kontingent
/// dieses Credentials ist aufgebraucht) lösen ein Failover aus. Beide sind
/// laut `harw_core::ModelError::is_retryable` **nicht** retryable — ein
/// erneuter Versuch mit demselben Credential würde denselben Fehler
/// reproduzieren, ein anderes Credential kann aber ein eigenes Kontingent
/// bzw. einen gültigen Schlüssel mitbringen. Rein transiente Fehler
/// (`Transient`, `Timeout`, generisches `RateLimited` aus einem 429 ohne
/// Kontingent-Code) bleiben bewusst außen vor, siehe Moduldoku.
///
/// # Returns
/// `true`, wenn `error` eine der beiden Varianten ist.
#[must_use]
pub(crate) fn should_failover(error: &ModelError) -> bool {
    matches!(
        error,
        ModelError::Auth { .. } | ModelError::QuotaExceeded { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_config::CredentialEntry;
    use std::collections::BTreeMap;

    fn sources(env_layer: &BTreeMap<String, String>) -> crate::SecretSources<'_> {
        crate::SecretSources {
            env_layer,
            resolver: None,
            home: None,
            endpoint: None,
        }
    }

    fn auth_with_pool(provider: &str, entries: Vec<CredentialEntry>) -> AuthConfig {
        let mut auth = AuthConfig::default();
        auth.credential_pool.insert(provider.to_owned(), entries);
        auth
    }

    fn entry(secret_ref: &str, priority: u32, label: Option<&str>) -> TestResult<CredentialEntry> {
        Ok(CredentialEntry {
            secret: secret_ref.parse().map_err(ctx("valid secret ref"))?,
            label: label.map(str::to_owned),
            priority,
            base_url: None,
        })
    }

    #[test]
    fn test_from_auth_config_returns_none_when_pool_absent() -> TestResult {
        let auth = AuthConfig::default();
        let env_layer = BTreeMap::new();
        let pool = CredentialPool::from_auth_config(&auth, "openai", sources(&env_layer), Ok)
            .map_err(ctx("resolves"))?;
        assert!(pool.is_none());
        Ok(())
    }

    #[test]
    fn test_from_auth_config_resolves_in_priority_order() -> TestResult {
        let mut env_layer = BTreeMap::new();
        env_layer.insert("OPENAI_A".to_owned(), "value-a".to_owned());
        env_layer.insert("OPENAI_B".to_owned(), "value-b".to_owned());
        let auth = auth_with_pool(
            "openai",
            vec![
                entry("env:OPENAI_B", 5, Some("second"))?,
                entry("env:OPENAI_A", 0, Some("first"))?,
            ],
        );
        let pool = CredentialPool::from_auth_config(&auth, "openai", sources(&env_layer), Ok)
            .map_err(ctx("resolves"))?
            .ok_or(TestError::Missing("pool present"))?;
        assert_eq!(pool.len(), 2);
        assert_eq!(pool.entry(0).label, "first");
        assert_eq!(pool.entry(1).label, "second");
        Ok(())
    }

    #[test]
    fn test_from_auth_config_synthesizes_label_when_missing() -> TestResult {
        let mut env_layer = BTreeMap::new();
        env_layer.insert("OPENAI_A".to_owned(), "value-a".to_owned());
        let auth = auth_with_pool("openai", vec![entry("env:OPENAI_A", 0, None)?]);
        let pool = CredentialPool::from_auth_config(&auth, "openai", sources(&env_layer), Ok)
            .map_err(ctx("resolves"))?
            .ok_or(TestError::Missing("pool present"))?;
        assert_eq!(pool.entry(0).label, "openai#0");
        Ok(())
    }

    fn cooldown_pool() -> CredentialPool<u32> {
        CredentialPool {
            entries: vec![
                PoolEntry {
                    value: 0,
                    base_url: None,
                    label: "a".to_owned(),
                },
                PoolEntry {
                    value: 1,
                    base_url: None,
                    label: "b".to_owned(),
                },
            ],
            cooldown_until: Mutex::new(vec![None, None]),
            cooldown: Duration::from_millis(20),
        }
    }

    #[test]
    fn test_select_returns_first_entry_when_nothing_cooling_down() {
        let pool = cooldown_pool();
        assert_eq!(pool.select(None), Some(0));
    }

    #[test]
    fn test_select_excludes_given_index() {
        let pool = cooldown_pool();
        assert_eq!(pool.select(Some(0)), Some(1));
    }

    #[test]
    fn test_mark_cooldown_skips_entry_until_it_expires() {
        let pool = cooldown_pool();
        pool.mark_cooldown(0);
        assert_eq!(pool.select(None), Some(1));
        std::thread::sleep(Duration::from_millis(25));
        assert_eq!(pool.select(None), Some(0));
    }

    #[test]
    fn test_select_fails_open_when_every_candidate_is_cooling_down() {
        let pool = cooldown_pool();
        pool.mark_cooldown(0);
        pool.mark_cooldown(1);
        // Beide kühlen ab: fail-open liefert trotzdem einen Index.
        assert_eq!(pool.select(None), Some(0));
        // Ausgeschlossener Index wird auch im fail-open-Pfad nie geliefert.
        assert_eq!(pool.select(Some(0)), Some(1));
    }

    #[test]
    fn test_prepend_primary_is_selected_before_pool_entries() {
        let pool = cooldown_pool().prepend_primary(9, "openai#primary".to_owned());
        assert_eq!(pool.len(), 3);
        assert_eq!(pool.entry(0).label, "openai#primary");
        assert_eq!(pool.entry(0).value, 9);
        assert!(pool.entry(0).base_url.is_none());
        // Frisch zusammengesetzt (kein Cooldown) -> Index 0 (primär) gewinnt,
        // nicht der frühere Index 0 des reinen Pools.
        assert_eq!(pool.select(None), Some(0));
    }

    #[test]
    fn test_prepend_primary_preserves_pool_entries_as_failover_in_order() {
        let pool = cooldown_pool().prepend_primary(9, "openai#primary".to_owned());
        // Bisherige Pool-Einträge ("a", "b") rücken unverändert relativ
        // zueinander auf Index 1 und 2.
        assert_eq!(pool.entry(1).label, "a");
        assert_eq!(pool.entry(2).label, "b");
    }

    #[test]
    fn test_prepend_primary_failover_falls_back_to_pool_entries_in_order() {
        let pool = cooldown_pool().prepend_primary(9, "openai#primary".to_owned());
        pool.mark_cooldown(0);
        assert_eq!(pool.select(None), Some(1));
        pool.mark_cooldown(1);
        assert_eq!(pool.select(None), Some(2));
    }

    #[test]
    fn test_should_failover_true_for_auth_and_quota() {
        assert!(should_failover(&ModelError::Auth {
            message: "denied".to_owned(),
        }));
        assert!(should_failover(&ModelError::QuotaExceeded {
            message: "exhausted".to_owned(),
        }));
    }

    #[test]
    fn test_should_failover_false_for_transient_and_timeout() {
        assert!(!should_failover(&ModelError::Transient {
            status: Some(503),
            retry_after_secs: None,
            message: "busy".to_owned(),
        }));
        assert!(!should_failover(&ModelError::Timeout {
            message: "deadline".to_owned(),
        }));
        assert!(!should_failover(&ModelError::RateLimited {
            retry_after_secs: 5,
            message: "429".to_owned(),
        }));
    }
}

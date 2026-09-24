//! Interaktiver Ersteinrichtungs-Assistent (Provider → Auth → Modell).
//!
//! Spec-Quelle: `docs/design/tui-architecture.md`,
//! Abschnitt SLICE 1 (Onboarding-OAuth/Paste-Fix) sowie
//! `docs/design/CONTRACT-setup-install.md`, Abschnitt „Crate
//! `harw-tui` (neue Datei `src/setup.rs`)".
//!
//! # Verantwortung
//! Dieses Modul besitzt die **I/O-freie** Zustandsmaschine der Ersteinrichtung
//! ([`SetupApp`]) sowie den dünnen [`ratatui`]-Loop [`run_setup`], der sie an
//! ein echtes Terminal koppelt. Die eigentlichen Übergänge (Provider-Auswahl,
//! Auth-Wahl, Modell-Wahl) liegen vollständig in [`SetupApp::on_key`] und sind
//! ohne TTY testbar. Credential-Erkennung wird an
//! [`harw_model_catalog::detect_local_sources`] delegiert; die
//! Terminal-Wiederherstellung an einen RAII-Guard.
//!
//! # Exportierte Typen
//! - [`SetupStage`] — Phase des Assistenten.
//! - [`SetupApp`] — die testbare Zustandsmaschine.
//! - [`SetupOutcome`] — das Ergebnis einer abgeschlossenen Einrichtung.
//!
//! # Concurrency
//! [`SetupApp`] ist ein reiner Wert-Typ ohne inneren geteilten Zustand.
//! [`run_setup`] läuft synchron im aufrufenden Thread und startet keine
//! weiteren Threads.
//!
//! # Fehler
//! Terminal-/Zeichen-Fehler werden als [`crate::TuiError`] ausgedrückt (aus
//! `app.rs` wiederverwendet).
//!
//! # Examples
//! ```
//! use harw_tui::{SetupApp, SetupStage};
//! use harw_model_catalog::{AuthMethod, ProviderApi, ProviderSpec};
//! use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
//!
//! let provider = ProviderSpec {
//!     id: "groq".to_owned(),
//!     name: "Groq".to_owned(),
//!     base_url: "https://api.groq.com/openai/v1".to_owned(),
//!     api: ProviderApi::OpenAiChat,
//!     auth: vec![AuthMethod::ApiKey { env_vars: vec!["GROQ_API_KEY".to_owned()] }],
//!     default_model: Some("llama-3.1-8b".to_owned()),
//!     featured: false,
//!     models: vec!["llama-3.1-8b".to_owned()],
//! };
//! let mut app = SetupApp::new(vec![provider]);
//! let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
//! app.on_key(press(KeyCode::Enter)); // Provider -> Endpoint
//! app.on_key(press(KeyCode::Enter)); // Endpoint -> Auth (nur Foundry/Custom fragen nach der API)
//! app.on_key(press(KeyCode::Char('k')));
//! app.on_key(press(KeyCode::Enter)); // Auth -> Model
//! app.on_key(press(KeyCode::Enter)); // Model -> Done
//! assert_eq!(app.outcome().map(|o| o.provider_id.as_str()), Some("groq"));
//! ```

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use harw_model_catalog::{AuthMethod, DetectedCredential, ProviderApi, ProviderSpec};

use crate::app::TuiError;
use crate::style;

/// Warnhinweis, wenn im Anthropic-Auth-Schritt eine Abo-OAuth-/Setup-Token-
/// Authentifizierung (statt eines API-Keys) gewählt oder eingefügt wird.
///
/// # Description
/// Wortgleich vorgegebener Hinweistext (siehe WARN-TUI-Auftrag). `harw-tui`
/// hängt nicht von `harw-provider-http` ab, daher ist der Text hier als
/// lokale Konstante dupliziert statt importiert.
const ANTHROPIC_OAUTH_WARNING: &str = "Hinweis: Du nutzt ein Abo-OAuth-/Setup-Token (Claude Free/Pro/Max) statt eines API-Keys. Laut Anthropics Nutzungsbedingungen ist Abo-OAuth für Claude Code und native Anthropic-Apps vorgesehen; Drittanbieter-Tools sollen API-Keys aus der Claude Console nutzen. Anthropic hat eine geplante Abrechnungsänderung für Drittanbieter-Nutzung im Juni 2026 vorerst pausiert, behält sich Durchsetzung aber ohne Vorankündigung vor – Anfragen können jederzeit abgelehnt werden. Nutzung auf eigene Gefahr. Stabil: API-Key (platform.claude.com). Quelle: https://code.claude.com/docs/en/legal-and-compliance";

/// Phase des Ersteinrichtungs-Assistenten.
///
/// # Description
/// Der Assistent durchläuft die Phasen strikt vorwärts:
/// `Provider → Auth → Model → Done`. Contract-Quelle: Abschnitt „harw-tui
/// (neue Datei `src/setup.rs`)".
///
/// # Concurrency
/// `Copy`-Wert ohne inneren Zustand; uneingeschränkt thread-sicher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupStage {
    /// Auswahl des Providers (Liste mit Tipp-Filter).
    Provider,
    /// Resource or gateway URL.
    Endpoint,
    /// Wire API for Foundry and custom endpoints.
    Api,
    /// Auswahl bzw. Eingabe der Authentifizierung.
    Auth,
    /// Auswahl des Modells.
    Model,
    /// Einrichtung abgeschlossen; [`SetupApp::outcome`] liefert das Ergebnis.
    Done,
}

/// Ergebnis einer abgeschlossenen Ersteinrichtung.
///
/// # Description
/// Trägt alle Felder, die der Aufrufer (z. B. `harw-cli`-Onboarding) benötigt,
/// um Provider- und Modell-Konfiguration zu persistieren. Contract-Quelle:
/// Abschnitt „harw-tui (neue Datei `src/setup.rs`)".
///
/// # Concurrency
/// Reiner Wert-Typ; `Send + Sync`, per `Clone` kopierbar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupOutcome {
    /// Gewählte Provider-Id (z. B. `"groq"`).
    pub provider_id: String,
    /// Basis-URL des gewählten Providers.
    pub base_url: String,
    /// Transport-/API-Familie als kebab-case-String (z. B. `"openai-chat"`).
    pub api: String,
    /// Gewähltes Modell (leer, wenn der Provider keine Modelle kennt).
    pub model: String,
    /// Referenz auf das gewählte Geheimnis, falls eines gewählt/getippt wurde.
    pub secret_ref: Option<String>,
    /// Explicit credential header scheme.
    pub auth_header: Option<String>,
}

/// Eine im Auth-Schritt auswählbare Authentifizierungsoption.
///
/// TTY-freies Zwischenergebnis der Zustandsmaschine.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AuthOption {
    /// Getippter API-Key; füllt den Eingabepuffer.
    ApiKey,
    /// Lokaler Provider ohne Geheimnis (nur Basis-URL).
    LocalBaseUrl,
    /// Benutzerdefiniertes Verfahren; kein Geheimnis im Assistenten.
    Custom,
    /// Erkannte lokale Credential-Quelle mit fertigem Ref-String.
    Detected(DetectedCredential),
    /// OAuth-/Setup-Token-Methode, die (noch) nicht lokal vorhanden ist —
    /// z. B. Claude-Setup-Token oder Codex-OAuth. Wird immer angezeigt, damit
    /// der OAuth-Weg sichtbar ist; der Token wird via Paste eingegeben und
    /// landet als Raw-Token in `secret_ref`.
    OAuth {
        /// Anzeige-Label (Quelle + Hinweis).
        label: String,
        /// Referenz-String, der als Fallback dient wenn kein Token eingegeben.
        secret_ref: String,
    },
}

impl AuthOption {
    /// Gibt das Anzeige-Label der Auth-Option zurück.
    fn label(&self) -> String {
        match self {
            AuthOption::ApiKey => "API-Key eingeben".to_owned(),
            AuthOption::LocalBaseUrl => "Lokal (nur Basis-URL)".to_owned(),
            AuthOption::Custom => "Benutzerdefiniert".to_owned(),
            AuthOption::Detected(detected) => {
                format!("Erkannt: {} ({})", detected.source.id, detected.secret_ref)
            }
            AuthOption::OAuth { label, .. } => label.clone(),
        }
    }

    /// Gibt zurück, ob diese Option ein Geheimnis via Eingabepuffer benötigt.
    fn is_secret(&self) -> bool {
        matches!(self, AuthOption::ApiKey | AuthOption::OAuth { .. })
    }
}

/// I/O-freie Zustandsmaschine der Ersteinrichtung.
///
/// # Description
/// Hält den Katalog, den aktuellen Schritt, den Tipp-Filter der Provider-Liste,
/// die berechneten Auth-/Modell-Optionen, das aktive Theme und das Endergebnis.
/// Alle Übergänge erfolgen ausschließlich über [`SetupApp::on_key`] und
/// [`SetupApp::on_paste`] und sind ohne Terminal testbar. Das Theme wird einmalig
/// beim Erzeugen via [`style::detect_theme`] bestimmt. Contract-Quelle:
/// Abschnitt „harw-tui (neue Datei `src/setup.rs`)" sowie Redesign-Spec SLICE 1
/// und SLICE 8.
///
/// # Concurrency
/// Reiner Wert-Typ; nicht geteilt, keine inneren Locks.
#[derive(Clone, Debug)]
pub struct SetupApp {
    /// Vollständiger Provider-Katalog.
    providers: Vec<ProviderSpec>,
    endpoint_input: String,
    model_input: String,
    auth_header: Option<String>,
    validation_error: String,
    /// Aktuelle Phase.
    stage: SetupStage,
    /// Tipp-Filter der Provider-Liste (case-insensitiv).
    filter: String,
    /// Aktuell markierter Index in der **gefilterten** Liste (Provider) bzw. in
    /// der Auth-/Modell-Optionsliste, je nach Phase.
    selected: usize,
    /// Vertikaler Scroll-Offset des Listenbereichs; folgt der Markierung.
    scroll: u16,
    /// Gewählter Provider (gesetzt beim Verlassen der Provider-Phase).
    chosen_provider: Option<ProviderSpec>,
    /// Berechnete Auth-Optionen der Auth-Phase.
    auth_options: Vec<AuthOption>,
    /// Eingabepuffer für einen getippten oder eingefügten API-Key bzw. Token.
    api_key: String,
    /// Ob der Nutzer gerade aktiv im Auth-Eingabefeld schreibt.
    ///
    /// `true`: Zeichen gehen direkt in `api_key`; `false`: Navigation aktiv.
    /// Wird bei Verlassen der Auth-Phase (begin_auth, begin_model) auf `false`
    /// zurückgesetzt.
    auth_editing: bool,
    /// Berechnete Modell-Optionen der Modell-Phase.
    model_options: Vec<String>,
    /// Ausgewählte Geheimnis-Referenz (falls vorhanden).
    secret_ref: Option<String>,
    /// Endergebnis, sobald die Phase [`SetupStage::Done`] erreicht ist.
    outcome: Option<SetupOutcome>,
    /// Aktives Terminal-Farbschema, einmalig beim Erzeugen erkannt. (SLICE 8)
    theme: style::Theme,
}

impl SetupApp {
    /// Baut einen Assistenten für den gegebenen Katalog.
    ///
    /// # Arguments
    /// - `catalog` (`Vec<ProviderSpec>`): der vollständige Provider-Katalog;
    ///   Eigentum wird übernommen.
    ///
    /// # Returns
    /// Ein [`SetupApp`] in Phase [`SetupStage::Provider`]. Das Theme wird via
    /// [`style::detect_theme`] aus der Prozessumgebung bestimmt.
    ///
    /// # Concurrency
    /// Rein; startet keine Threads.
    #[must_use]
    pub fn new(catalog: Vec<ProviderSpec>) -> Self {
        Self {
            providers: catalog,
            endpoint_input: String::new(),
            model_input: String::new(),
            auth_header: None,
            validation_error: String::new(),
            stage: SetupStage::Provider,
            filter: String::new(),
            selected: 0,
            scroll: 0,
            chosen_provider: None,
            auth_options: Vec::new(),
            api_key: String::new(),
            auth_editing: false,
            model_options: Vec::new(),
            secret_ref: None,
            outcome: None,
            theme: style::detect_theme(),
        }
    }

    /// Gibt die aktuelle Phase zurück.
    #[must_use]
    pub fn stage(&self) -> SetupStage {
        self.stage
    }

    /// Gibt das Endergebnis zurück, falls die Einrichtung abgeschlossen ist.
    ///
    /// # Returns
    /// `Some(&SetupOutcome)` in Phase [`SetupStage::Done`], sonst `None`.
    #[must_use]
    pub fn outcome(&self) -> Option<&SetupOutcome> {
        self.outcome.as_ref()
    }

    /// Gibt zurück, ob der Nutzer gerade im Auth-Eingabefeld tippt.
    ///
    /// # Returns
    /// `true` wenn `stage == Auth` und `auth_editing == true`.
    ///
    /// # Concurrency
    /// Rein; liest nur den eigenen Zustand.
    #[must_use]
    pub fn is_auth_editing(&self) -> bool {
        self.auth_editing
    }

    /// Bricht die Auth-Eingabe ab: leert den Puffer und setzt den
    /// Navigationsmodus zurück.
    ///
    /// # Description
    /// Wird vom Event-Loop bei `Esc` während des Editing-Modus aufgerufen,
    /// um nur die Eingabe zu verwerfen — nicht den gesamten Assistenten.
    ///
    /// # Concurrency
    /// Rein; verändert nur den eigenen Zustand.
    pub fn cancel_auth_editing(&mut self) {
        self.auth_editing = false;
        self.api_key.clear();
    }

    /// Verarbeitet einen Tasten-Anschlag und treibt die Zustandsmaschine.
    ///
    /// # Description
    /// I/O-freier Übergang. Nicht-`Press`-Ereignisse werden ignoriert. Je nach
    /// Phase navigieren `Up`/`Down`, filtert bzw. tippt `Char`, korrigiert
    /// `Backspace` und bestätigt `Enter`. In der Auth-Phase unterstützt ein
    /// Editing-Submodus (`auth_editing`) direktes Tippen in den Key-Puffer mit
    /// Sofort-Routing via Zifferntasten. Contract-Quelle: Redesign-Spec SLICE 1.
    ///
    /// # Arguments
    /// - `key` (`crossterm::event::KeyEvent`): der zu verarbeitende Anschlag.
    ///
    /// # Concurrency
    /// Rein; verändert nur den eigenen Zustand.
    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        match self.stage {
            SetupStage::Provider => self.on_key_provider(key.code),
            SetupStage::Endpoint => self.on_key_endpoint(key.code),
            SetupStage::Api => self.on_key_api(key.code),
            SetupStage::Auth => self.on_key_auth(key.code),
            SetupStage::Model => self.on_key_model(key.code),
            SetupStage::Done => {}
        }
    }

    /// Verarbeitet einen Paste-Event und füllt den Auth-Eingabepuffer.
    ///
    /// # Description
    /// Wird vom Event-Loop bei `Event::Paste` aufgerufen. Wenn sich die App in
    /// der Auth-Phase befindet und die aktuell markierte Option ein Geheimnis
    /// erwartet (ApiKey oder OAuth), wird der eingefügte Text getrimmt und als
    /// neuer Wert des Eingabepuffers gesetzt. Der `auth_editing`-Modus wird
    /// dabei aktiviert. Contract-Quelle: Redesign-Spec SLICE 1, Punkt d.
    ///
    /// # Arguments
    /// - `pasted` (`String`): der eingefügte Rohtext (inklusive möglicher
    ///   Leerzeichen/Zeilenenden).
    ///
    /// # Concurrency
    /// Rein; verändert nur den eigenen Zustand.
    pub fn on_paste(&mut self, pasted: String) {
        if self.stage == SetupStage::Endpoint {
            self.endpoint_input = pasted.trim().to_owned();
        } else if self.stage == SetupStage::Model {
            self.model_input = pasted.trim().to_owned();
        } else if self.stage == SetupStage::Auth && self.selected_is_secret() {
            self.auth_editing = true;
            self.api_key = pasted.trim().to_owned();
        }
    }

    /// Verarbeitet Tasten in der Provider-Phase.
    fn on_key_provider(&mut self, code: KeyCode) {
        let count = self.filtered_indices().len();
        match code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down if count > 0 && self.selected + 1 < count => {
                self.selected += 1;
            }
            KeyCode::Char(character) => {
                self.filter.push(character);
                self.clamp_selected();
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.clamp_selected();
            }
            KeyCode::Enter => {
                let filtered = self.filtered_indices();
                if let Some(&index) = filtered.get(self.selected) {
                    let provider = self.providers[index].clone();
                    self.endpoint_input = if provider.base_url.contains('<') {
                        String::new()
                    } else {
                        provider.base_url.clone()
                    };
                    self.chosen_provider = Some(provider);
                    self.stage = SetupStage::Endpoint;
                    self.selected = 0;
                    self.validation_error.clear();
                }
            }
            _ => {}
        }
    }

    fn on_key_endpoint(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char(c) => self.endpoint_input.push(c),
            KeyCode::Backspace => {
                self.endpoint_input.pop();
            }
            KeyCode::Enter => {
                let endpoint = self.endpoint_input.trim().trim_end_matches('/');
                if !(endpoint.starts_with("https://") || endpoint.starts_with("http://"))
                    || endpoint.contains(['<', '>', ' ', '?', '#'])
                    || endpoint.ends_with("://")
                {
                    self.validation_error =
                        "Vollständige HTTP(S)-Basis-URL ohne Platzhalter eingeben.".into();
                    return;
                }
                let Some(provider) = self.chosen_provider.as_mut() else {
                    return;
                };
                provider.base_url = endpoint.to_owned();
                self.validation_error.clear();
                if provider.id == "foundry" || provider.id == "custom" {
                    self.stage = SetupStage::Api;
                    self.selected = 0;
                } else {
                    let provider = provider.clone();
                    self.begin_auth(provider);
                }
            }
            _ => {}
        }
    }

    fn on_key_api(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(5),
            KeyCode::Enter => {
                let Some(provider) = self.chosen_provider.as_mut() else {
                    return;
                };
                provider.api = match self.selected % 3 {
                    0 => ProviderApi::OpenAiChat,
                    1 => ProviderApi::OpenAiResponses,
                    _ => ProviderApi::AnthropicMessages,
                };
                self.auth_header = Some(
                    if self.selected >= 3 {
                        "bearer"
                    } else if provider.api == ProviderApi::AnthropicMessages {
                        "x-api-key"
                    } else if provider.id == "foundry" {
                        "api-key"
                    } else {
                        "bearer"
                    }
                    .into(),
                );
                if provider.id == "foundry" {
                    let endpoint = provider.base_url.trim_end_matches('/');
                    let root = endpoint
                        .trim_end_matches("/openai/v1")
                        .trim_end_matches("/anthropic/v1/messages")
                        .trim_end_matches("/anthropic/v1")
                        .trim_end_matches("/anthropic");
                    provider.base_url = format!(
                        "{root}/{}",
                        if provider.api == ProviderApi::AnthropicMessages {
                            "anthropic"
                        } else {
                            "openai/v1"
                        }
                    );
                    // A public model list cannot identify resource deployment names.
                    provider.models.clear();
                    provider.default_model = None;
                }
                let provider = provider.clone();
                self.begin_auth(provider);
            }
            _ => {}
        }
    }

    /// Verarbeitet Tasten in der Auth-Phase mit Editing-Submodus.
    ///
    /// # Description
    /// Zwei Sub-Modi:
    /// - **Navigations-Modus** (`auth_editing=false`): `Up`/`Down` navigieren
    ///   die Liste; eine Zifferntaste `'1'–'9'` bei leerem Puffer wählt sofort
    ///   und wechselt für ApiKey/OAuth in den Editing-Modus; andere Zeichen
    ///   beginnen die Eingabe wenn eine Secret-Option aktiv ist; `Enter` startet
    ///   bei leerem Secret-Puffer die Eingabe oder bestätigt die Wahl.
    /// - **Editing-Modus** (`auth_editing=true`): Zeichen werden in den Puffer
    ///   geschrieben; `Backspace` löscht; `Enter` bestätigt; `Up`/`Down`
    ///   verlassen den Editing-Modus ohne Bestätigung.
    ///
    /// Contract-Quelle: Redesign-Spec SLICE 1, Punkt c.
    fn on_key_auth(&mut self, code: KeyCode) {
        if self.auth_editing {
            // Editing-Submodus: Zeichen direkt in den Puffer, Navigation verlässt
            // den Editing-Modus ohne Bestätigung.
            match code {
                KeyCode::Char(c) => self.api_key.push(c),
                KeyCode::Backspace => {
                    self.api_key.pop();
                }
                KeyCode::Enter => self.confirm_auth(),
                KeyCode::Up | KeyCode::Down => self.auth_editing = false,
                _ => {}
            }
        } else {
            // Navigations-Modus: Direktwahl via Ziffer oder Zeicheneingabe startet
            // den Editing-Modus für Secret-Optionen.
            let selected_is_secret = self.selected_is_secret();
            match code {
                KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down if self.selected + 1 < self.auth_options.len() => {
                    self.selected += 1;
                }
                // Sofort-Routing: Zifferntaste wählt Option direkt (nur wenn Puffer leer).
                KeyCode::Char(c @ '1'..='9') if self.api_key.is_empty() => {
                    let idx = (c as usize) - ('1' as usize);
                    if idx < self.auth_options.len() {
                        self.selected = idx;
                        if self.selected_is_secret() {
                            self.auth_editing = true;
                        } else {
                            self.confirm_auth();
                        }
                    }
                }
                // Beliebiges Zeichen: wenn Secret-Option aktiv, Editing-Modus starten
                // und Zeichen sofort in Puffer aufnehmen.
                KeyCode::Char(c) if selected_is_secret => {
                    self.auth_editing = true;
                    self.api_key.push(c);
                }
                // Enter: bei leerem Secret-Puffer Editing-Modus starten, sonst bestätigen.
                KeyCode::Enter => {
                    if selected_is_secret && self.api_key.is_empty() {
                        self.auth_editing = true;
                    } else {
                        self.confirm_auth();
                    }
                }
                _ => {}
            }
        }
    }

    /// Verarbeitet Tasten in der Modell-Phase.
    fn on_key_model(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down if self.selected + 1 < self.model_options.len() => {
                self.selected += 1;
            }
            KeyCode::Char(c) => self.model_input.push(c),
            KeyCode::Backspace => {
                self.model_input.pop();
            }
            KeyCode::Enter => self.finish(),
            _ => {}
        }
    }

    /// Gibt zurück, ob die aktuell markierte Auth-Option ein Geheimnis via
    /// Eingabepuffer erwartet (ApiKey oder OAuth).
    fn selected_is_secret(&self) -> bool {
        self.auth_options
            .get(self.selected)
            .map(AuthOption::is_secret)
            .unwrap_or(false)
    }

    /// Gibt zurück, ob die aktuell in der Auth-Phase markierte bzw.
    /// eingegebene Authentifizierung eine Anthropic-Abo-OAuth-/Setup-Token-
    /// Nutzung darstellt.
    ///
    /// # Description
    /// Greift nur für den Anthropic-Provider (`provider.id == "anthropic"`,
    /// API [`ProviderApi::AnthropicMessages`]). Erkennt: die Auswahl einer
    /// OAuth-/Setup-Token-Option ([`AuthOption::OAuth`]) oder einer bereits
    /// erkannten lokalen Quelle ([`AuthOption::Detected`]), deren Referenz auf
    /// `~/.claude/.credentials.json` oder das Setup-Token-Environment
    /// verweist, sowie einen getippten/eingefügten API-Key, der mit
    /// `sk-ant-oat` beginnt oder ebenfalls auf diese Quellen referenziert.
    ///
    /// # Returns
    /// `true`, wenn die aktuelle Auswahl/Eingabe eine solche Nutzung ist.
    ///
    /// # Concurrency
    /// Rein; liest nur den eigenen Zustand.
    fn is_anthropic_oauth_selection(&self) -> bool {
        let Some(provider) = self.chosen_provider.as_ref() else {
            return false;
        };
        if self.stage != SetupStage::Auth
            || provider.id != "anthropic"
            || provider.api != ProviderApi::AnthropicMessages
        {
            return false;
        }
        let looks_like_oauth_ref = |value: &str| {
            let trimmed = value.trim();
            trimmed.starts_with("sk-ant-oat")
                || trimmed.contains(".credentials.json")
                || trimmed.contains("CLAUDE_CODE_OAUTH_TOKEN")
        };
        match self.auth_options.get(self.selected) {
            Some(AuthOption::OAuth { secret_ref, .. }) => {
                looks_like_oauth_ref(secret_ref) || looks_like_oauth_ref(&self.api_key)
            }
            Some(AuthOption::Detected(detected)) => looks_like_oauth_ref(&detected.secret_ref),
            Some(AuthOption::ApiKey) => looks_like_oauth_ref(&self.api_key),
            _ => false,
        }
    }

    /// Gibt den Anthropic-OAuth-Warnhinweis zurück, wenn die aktuell
    /// markierte bzw. eingegebene Auth-Option eine Abo-OAuth-/Setup-Token-
    /// Nutzung für Anthropic darstellt, sonst `None`.
    ///
    /// # Description
    /// Reine Anzeige-Ableitung ohne gespeicherten Zustand: verschwindet
    /// automatisch, sobald der Nutzer auf eine reine API-Key-Auswahl (ohne
    /// OAuth-artigen Wert) wechselt. Contract-Quelle: WARN-TUI-Auftrag
    /// (Setup-Warnung bei Anthropic-Bearer-/OAuth-Auswahl).
    ///
    /// # Returns
    /// `Some(text)` mit dem wortgleich vorgegebenen Warnhinweis, sonst `None`.
    ///
    /// # Concurrency
    /// Rein; liest nur den eigenen Zustand.
    #[must_use]
    pub fn oauth_warning(&self) -> Option<&'static str> {
        if self.is_anthropic_oauth_selection() {
            Some(ANTHROPIC_OAUTH_WARNING)
        } else {
            None
        }
    }

    /// Berechnet die Indizes der Provider, die den Filter erfüllen.
    fn filtered_indices(&self) -> Vec<usize> {
        let needle = self.filter.to_lowercase();
        self.providers
            .iter()
            .enumerate()
            .filter(|(_, provider)| {
                needle.is_empty()
                    || provider.id.to_lowercase().contains(&needle)
                    || provider.name.to_lowercase().contains(&needle)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Klemmt den Auswahlindex in die Grenzen der gefilterten Liste.
    fn clamp_selected(&mut self) {
        let count = self.filtered_indices().len();
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }

    /// Wechselt in die Auth-Phase (oder überspringt sie, wenn keine Option
    /// existiert) für den gewählten Provider.
    fn begin_auth(&mut self, provider: ProviderSpec) {
        let options = if provider.id == "custom" {
            vec![AuthOption::ApiKey, AuthOption::Custom]
        } else {
            build_auth_options(&provider)
        };
        self.chosen_provider = Some(provider);
        self.selected = 0;
        self.api_key.clear();
        self.auth_editing = false;
        self.secret_ref = None;
        if options.is_empty() {
            self.auth_options = options;
            self.begin_model();
        } else {
            self.auth_options = options;
            self.stage = SetupStage::Auth;
        }
    }

    /// Bestätigt die aktuell markierte Auth-Option und wechselt zur Modellwahl.
    ///
    /// # Description
    /// Für OAuth: wenn ein Token in `api_key` vorhanden ist, wird er als
    /// `secret_ref` übernommen; andernfalls dient der Katalog-Ref als Fallback.
    /// Contract-Quelle: Redesign-Spec SLICE 1, Punkt e.
    fn confirm_auth(&mut self) {
        let Some(option) = self.auth_options.get(self.selected) else {
            return;
        };
        self.secret_ref = match option {
            AuthOption::Detected(detected) => Some(detected.secret_ref.clone()),
            AuthOption::OAuth { secret_ref, .. } => {
                // Wenn der Nutzer einen Token eingegeben/eingefügt hat, hat dieser
                // Vorrang vor dem Katalog-Ref.
                if self.api_key.is_empty() {
                    Some(secret_ref.clone())
                } else {
                    Some(self.api_key.trim().to_owned())
                }
            }
            AuthOption::ApiKey => {
                if self.api_key.is_empty() {
                    None
                } else {
                    Some(self.api_key.clone())
                }
            }
            AuthOption::LocalBaseUrl | AuthOption::Custom => {
                self.auth_header = Some("none".into());
                None
            }
        };
        if self.selected_is_secret()
            && self
                .secret_ref
                .as_deref()
                .is_none_or(|s| s.trim().is_empty())
        {
            self.validation_error = "API-Key oder Secret-Referenz erforderlich.".into();
            return;
        }
        self.validation_error.clear();
        self.auth_editing = false;
        self.begin_model();
    }

    /// Wechselt in die Modell-Phase und berechnet die Modell-Optionen.
    fn begin_model(&mut self) {
        self.model_options = self
            .chosen_provider
            .as_ref()
            .map(model_options_for)
            .unwrap_or_default();
        self.selected = 0;
        self.stage = SetupStage::Model;
    }

    /// Schließt die Einrichtung ab und baut das [`SetupOutcome`].
    fn finish(&mut self) {
        let Some(provider) = self.chosen_provider.as_ref() else {
            return;
        };
        let model = if self.model_input.trim().is_empty() {
            self.model_options
                .get(self.selected)
                .cloned()
                .unwrap_or_default()
        } else {
            self.model_input.trim().to_owned()
        };
        if model.is_empty() {
            self.validation_error = "Modell-ID / Deployment-Name erforderlich.".into();
            return;
        }
        self.outcome = Some(SetupOutcome {
            provider_id: provider.id.clone(),
            base_url: provider.base_url.clone(),
            api: api_str(provider.api).to_owned(),
            model,
            secret_ref: self.secret_ref.clone(),
            auth_header: self.auth_header.clone(),
        });
        self.stage = SetupStage::Done;
    }
}

/// Baut die Auth-Optionen für einen Provider.
///
/// # Description
/// Reihenfolge:
/// 1. Erkannte, lokal vorhandene Credential-Quellen (sofort nutzbar).
/// 2. Im Katalog deklarierte Auth-Methoden (ApiKey, LocalBaseUrl, Custom).
/// 3. OAuth-/Setup-Token-Wege für **nicht** vorhandene Quellen — immer
///    sichtbar, damit der OAuth-Weg auch ohne lokale Installation zugänglich
///    ist. Der `has_import`-Zwang des ursprünglichen Codes wurde entfernt:
///    OAuth-Optionen werden für ALLE erkannten Quellen mit `exists=false`
///    eingeblendet, unabhängig davon, ob der Provider eine `LocalImport`-
///    Methode deklariert.
///
/// # Arguments
/// - `provider` (`&ProviderSpec`): der gewählte Provider.
///
/// # Returns
/// Liste aller verfügbaren Auth-Optionen.
fn build_auth_options(provider: &ProviderSpec) -> Vec<AuthOption> {
    let detected = harw_model_catalog::detect_local_sources(&provider.id);

    // 1. Bereits vorhandene lokale Quellen (sofort nutzbar), zuerst.
    let mut options: Vec<AuthOption> = detected
        .iter()
        .filter(|d| d.exists)
        .cloned()
        .map(AuthOption::Detected)
        .collect();

    // 2. Deklarierte Katalog-Methoden.
    for method in &provider.auth {
        match method {
            AuthMethod::ApiKey { .. } => {
                if !options.contains(&AuthOption::ApiKey) {
                    options.push(AuthOption::ApiKey);
                }
            }
            AuthMethod::LocalBaseUrl => {
                if !options.contains(&AuthOption::LocalBaseUrl) {
                    options.push(AuthOption::LocalBaseUrl);
                }
            }
            AuthMethod::Custom => {
                if !options.contains(&AuthOption::Custom) {
                    options.push(AuthOption::Custom);
                }
            }
            AuthMethod::LocalImport { .. } => {}
        }
    }

    // 3. OAuth-/Setup-Token-Wege IMMER sichtbar machen — der `has_import`-
    //    Zwang wurde entfernt, damit anthropic/openai und zukünftige Provider
    //    OAuth-Quellen auch ohne LocalImport-Deklaration anzeigen.
    for source in detected.iter().filter(|d| !d.exists) {
        let label = format!(
            "OAuth/Setup-Token: {} (via `harw auth login`)",
            source.source.id
        );
        let option = AuthOption::OAuth {
            label,
            secret_ref: source.secret_ref.clone(),
        };
        if !options.contains(&option) {
            options.push(option);
        }
    }

    options
}

/// Berechnet die Modell-Optionen eines Providers.
///
/// Nutzt `models`, fällt auf `default_model` zurück und liefert sonst eine
/// leere Liste.
fn model_options_for(provider: &ProviderSpec) -> Vec<String> {
    if !provider.models.is_empty() {
        provider.models.clone()
    } else if let Some(default) = &provider.default_model {
        vec![default.clone()]
    } else {
        Vec::new()
    }
}

/// Wandelt [`ProviderApi`] in seinen kebab-case-String.
fn api_str(api: ProviderApi) -> &'static str {
    match api {
        ProviderApi::OpenAiResponses => "openai-responses",
        ProviderApi::OpenAiChat => "openai-chat",
        ProviderApi::AnthropicMessages => "anthropic-messages",
        ProviderApi::Ollama => "ollama",
    }
}

/// RAII-Guard, der Raw-Mode, Bracketed-Paste und Alternate-Screen bei Drop
/// zurückstellt.
///
/// # Description
/// Aktiviert Raw-Mode, Alternate-Screen und `EnableBracketedPaste` in der
/// richtigen Reihenfolge. Der Drop-Handler deaktiviert in umgekehrter
/// Reihenfolge. `EnterAlternateScreen` bleibt für SLICE 1 unverändert
/// (Entfernung folgt in SLICE 2). Contract-Quelle: Redesign-Spec SLICE 1,
/// Punkt a.
///
/// # Concurrency
/// Nicht thread-sicher; ausschließlich vom Renderer-Thread verwendet.
pub(crate) struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    /// Aktiviert Raw-Mode, Alternate-Screen, Bracketed-Paste und Mouse-Capture
    /// und baut das ratatui-Terminal.
    ///
    /// # Errors
    /// [`TuiError::Io`], wenn Terminal-Setup fehlschlägt.
    pub(crate) fn enter() -> Result<Self, TuiError> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = crossterm::execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(TuiError::from(error));
        }
        if let Err(error) = crossterm::execute!(stdout, EnableBracketedPaste) {
            let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
            let _ = disable_raw_mode();
            return Err(TuiError::from(error));
        }
        // Mouse-Capture liefert die Radbewegungen, mit denen die Chat-Historie
        // gescrollt wird. Kein harter Fehlerfall: ein Terminal ohne
        // Maus-Unterstützung soll die TUI trotzdem starten, dort bleibt das
        // Scrollen per Tastatur.
        if let Err(error) = crossterm::execute!(stdout, EnableMouseCapture) {
            tracing::warn!(%error, "tui.mouse_capture_unavailable");
        }
        let backend = CrosstermBackend::new(stdout);
        match Terminal::new(backend) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = crossterm::execute!(io::stdout(), DisableBracketedPaste);
                let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
                let _ = disable_raw_mode();
                Err(TuiError::from(error))
            }
        }
    }

    /// Liefert eine veränderbare Referenz auf das zugrunde liegende Terminal.
    pub(crate) fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Umgekehrte Reihenfolge: zuerst Maus und Paste deaktivieren, dann Alt-Screen,
        // dann Raw-Mode. Best-effort — Fehler dürfen kein Panic auslösen.
        let _ = crossterm::execute!(self.terminal.backend_mut(), DisableMouseCapture);
        let _ = crossterm::execute!(self.terminal.backend_mut(), DisableBracketedPaste);
        let _ = crossterm::execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
        let _ = self.terminal.show_cursor();
    }
}

/// Startet den interaktiven Ersteinrichtungs-Assistenten.
///
/// # Description
/// Aktiviert Raw-Mode/Alternate-Screen/BracketedPaste hinter einem RAII-Guard
/// und betreibt den synchronen Event-Loop über [`crossterm::event`]. Paste-
/// Events werden an [`SetupApp::on_paste`] weitergereicht; Tastenanschläge an
/// [`SetupApp::on_key`]. `Esc` im Navigations-Modus bricht ab (`Ok(None)`);
/// `Esc` im Auth-Editing-Modus leert nur den Puffer. Das Erreichen von
/// [`SetupStage::Done`] liefert `Ok(Some(outcome))`. Contract-Quelle: Abschnitt
/// „harw-tui (neue Datei `src/setup.rs`)".
///
/// # Arguments
/// - `catalog` (`Vec<ProviderSpec>`): der zu präsentierende Provider-Katalog.
///
/// # Returns
/// `Ok(Some(SetupOutcome))` bei Abschluss, `Ok(None)` bei Abbruch.
///
/// # Errors
/// - [`TuiError::Io`]: bei Terminal-Setup, Zeichnen oder Event-I/O.
///
/// # Concurrency
/// Läuft synchron im aufrufenden Thread; startet keine weiteren Threads.
pub fn run_setup(catalog: Vec<ProviderSpec>) -> Result<Option<SetupOutcome>, TuiError> {
    let mut app = SetupApp::new(catalog);
    let mut guard = TerminalGuard::enter()?;
    let result = setup_loop(&mut guard, &mut app);
    drop(guard);
    result
}

/// Betreibt den synchronen Setup-Event-Loop bis Abschluss oder Abbruch.
///
/// # Description
/// Verarbeitet `Event::Paste` (→ `on_paste`) und `Event::Key` (→ `on_key`).
/// `Esc` im Auth-Editing-Modus bricht nur die Eingabe ab; außerhalb des
/// Editing-Modus beendet `Esc` den gesamten Assistenten. Contract-Quelle:
/// Redesign-Spec SLICE 1, Punkte g und h.
fn setup_loop(
    guard: &mut TerminalGuard,
    app: &mut SetupApp,
) -> Result<Option<SetupOutcome>, TuiError> {
    loop {
        draw(guard, app)?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }

        match event::read()? {
            Event::Paste(text) => {
                app.on_paste(text);
                if app.outcome().is_some() {
                    return Ok(app.outcome().cloned());
                }
            }
            Event::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                if key.code == KeyCode::Esc {
                    // Im Auth-Editing-Modus: nur Puffer leeren, nicht abbrechen.
                    if app.stage == SetupStage::Auth && app.is_auth_editing() {
                        app.cancel_auth_editing();
                    } else {
                        return Ok(None);
                    }
                    continue;
                }
                app.on_key(key);
                if app.outcome().is_some() {
                    return Ok(app.outcome().cloned());
                }
            }
            _ => {}
        }
    }
}

/// Zeichnet einen Frame: Kopfzeile mit Phase plus phasenspezifische Liste.
///
/// Auswahl-Stile und Hinweiszeilen werden theme-abhängig über [`crate::style`]
/// aufgelöst (SLICE 8).
fn draw(guard: &mut TerminalGuard, app: &mut SetupApp) -> Result<(), TuiError> {
    guard
        .terminal()
        .draw(|frame| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(1)])
                .split(frame.area());

            let header = Paragraph::new(format!("harw setup — {:?}  (Esc bricht ab)", app.stage))
                .block(Block::default().borders(Borders::ALL).title("Einrichtung"));
            frame.render_widget(header, chunks[0]);

            let lines = body_lines(app);
            let mut body = Paragraph::new(lines).block(Block::default().borders(Borders::ALL));
            // Listen-Phasen: eine Zeile je Eintrag (kein Umbruch), damit der
            // Zeilenindex der Markierung stimmt und der Offset ihr folgen kann.
            if let Some(line) = selected_line_index(app) {
                // Rahmen oben/unten abziehen.
                let viewport = usize::from(chunks[1].height.saturating_sub(2)).max(1);
                app.scroll = follow_scroll(usize::from(app.scroll), line, viewport);
            } else {
                app.scroll = 0;
                body = body.wrap(Wrap { trim: false });
            }
            frame.render_widget(body.scroll((app.scroll, 0)), chunks[1]);
        })
        .map_err(TuiError::from)?;
    Ok(())
}

/// Zeilenindex der markierten Zeile in [`body_lines`], falls die Phase eine Liste ist.
fn selected_line_index(app: &SetupApp) -> Option<usize> {
    match app.stage {
        // Kopfzeile „Filter: …“.
        SetupStage::Provider => Some(1 + app.selected),
        SetupStage::Api => Some(app.selected),
        // Kopfzeile „[1-9] Direktwahl“.
        SetupStage::Auth => Some(1 + app.selected),
        // Hinweis, eigene Auswahl, Validierungsfehler.
        SetupStage::Model => Some(3 + app.selected),
        SetupStage::Endpoint | SetupStage::Done => None,
    }
}

/// Minimal verschobener Scroll-Offset, der `line` im Sichtfenster hält.
fn follow_scroll(current: usize, line: usize, viewport: usize) -> u16 {
    let offset = if line < current {
        line
    } else if line >= current + viewport {
        line + 1 - viewport
    } else {
        current
    };
    u16::try_from(offset).unwrap_or(u16::MAX)
}

/// Baut die phasenspezifischen Anzeigezeilen des Frames.
///
/// # Description
/// In der Auth-Phase wird das Eingabefeld angezeigt, wenn der Nutzer sich im
/// Editing-Modus befindet oder eine Secret-Option (ApiKey/OAuth) ausgewählt
/// ist. Das Label unterscheidet zwischen ApiKey und OAuth. Auswahl-Hervorhebung
/// und Hinweiszeilen werden über [`crate::style`] theme-abhängig aufgelöst
/// (SLICE 8). Contract-Quelle: Redesign-Spec SLICE 1, Punkt g.
fn body_lines(app: &SetupApp) -> Vec<Line<'static>> {
    let theme = app.theme;
    match app.stage {
        SetupStage::Provider => {
            let mut lines = vec![Line::from(format!("Filter: {}", app.filter))];
            let filtered = app.filtered_indices();
            for (row, &index) in filtered.iter().enumerate() {
                let provider = &app.providers[index];
                lines.push(selectable_line(
                    row == app.selected,
                    format!("{} ({})", provider.name, provider.id),
                    theme,
                ));
            }
            lines
        }
        SetupStage::Endpoint => vec![
            Line::from("Ressourcen-/Gateway-Basis-URL (bearbeiten oder einfügen):"),
            Line::from(app.endpoint_input.clone()),
            Line::from("Foundry: https://<ressource>.services.ai.azure.com — keine Projekt-URL"),
            Line::from(app.validation_error.clone()),
        ],
        SetupStage::Api => [
            "Chat Completions — API-Key (GPT, Llama, Mistral, DeepSeek, …)",
            "Responses — API-Key (unterstützte GPT-Deployments)",
            "Anthropic Messages — API-Key (Claude)",
            "Chat Completions — Entra/Bearer-Token-Referenz",
            "Responses — Entra/Bearer-Token-Referenz",
            "Anthropic Messages — Entra/Bearer-Token-Referenz",
        ]
        .iter()
        .enumerate()
        .map(|(i, label)| selectable_line(i == app.selected, (*label).into(), theme))
        .collect(),
        SetupStage::Auth => {
            let mut lines = Vec::new();

            // Direktwahl-Hinweis — gedimmter Stil via style::dim_style().
            lines.push(Line::from(Span::styled(
                "  [1-9] Direktwahl",
                style::dim_style(theme),
            )));

            for (row, option) in app.auth_options.iter().enumerate() {
                lines.push(selectable_line(row == app.selected, option.label(), theme));
            }

            // Eingabefeld anzeigen wenn Editing-Modus aktiv ODER Secret-Option gewählt.
            let selected_is_secret = app
                .auth_options
                .get(app.selected)
                .map(AuthOption::is_secret)
                .unwrap_or(false);

            if app.auth_editing || selected_is_secret {
                let label: &str = match app.auth_options.get(app.selected) {
                    Some(AuthOption::OAuth { .. }) => "Setup-Token (einfügen mit Ctrl+V):",
                    _ if app.auth_header.as_deref() == Some("bearer") => {
                        "Token / Referenz (z.B. env:AZURE_AUTH_TOKEN):"
                    }
                    _ => "API-Key / Secret-Referenz:",
                };
                // Eingabefeld in der Akzentfarbe des aktiven Themes.
                lines.push(Line::from(Span::styled(
                    format!("{} {}▌", label, mask(&app.api_key)),
                    Style::default().fg(style::accent_color(theme)),
                )));
                // Hint-Zeile gedimmt.
                lines.push(Line::from(Span::styled(
                    "  (tippen oder Ctrl+V zum Einfügen, Enter zum Bestätigen)",
                    style::dim_style(theme),
                )));
            }

            // Anthropic-Abo-OAuth-Warnung: erscheint bei Auswahl/Eingabe einer
            // Bearer-/OAuth-Authentifizierung, verschwindet bei Wechsel auf
            // eine reine API-Key-Auswahl. Wortumbruch auf Leerzeichen, damit
            // der lange Hinweis in der schmalen Liste lesbar bleibt — der
            // Text selbst bleibt dabei wortgleich.
            if let Some(warning) = app.oauth_warning() {
                let warn_style = Style::default().fg(style::warning_color(theme));
                for chunk in wrap_warning(warning, 76) {
                    lines.push(Line::from(Span::styled(chunk, warn_style)));
                }
            }

            lines
        }
        SetupStage::Model => {
            let mut lines = vec![
                Line::from("Modell wählen oder eigene Modell-ID / Azure-Deployment-Namen tippen:"),
                Line::from(format!("Eigene Auswahl: {}", app.model_input)),
                Line::from(app.validation_error.clone()),
            ];
            lines.extend(
                app.model_options
                    .iter()
                    .enumerate()
                    .map(|(row, model)| selectable_line(row == app.selected, model.clone(), theme)),
            );
            lines
        }
        SetupStage::Done => vec![Line::from("Einrichtung abgeschlossen.")],
    }
}

/// Baut eine markierbare Listenzeile (mit `›`-Präfix und Hervorhebung).
///
/// # Description
/// Für markierte Zeilen wird [`style::selected_style`] verwendet (theme-abhängige
/// Akzentfarbe + BOLD); unmarkierte Zeilen erhalten den Standard-Stil.
/// Spec-Quelle: SLICE 8.
///
/// # Arguments
/// - `selected` (`bool`): ob diese Zeile aktuell markiert ist.
/// - `text` (`String`): der anzuzeigende Text.
/// - `theme` (`style::Theme`): das aktive Farbschema.
///
/// # Returns
/// Eine formatierte [`Line`] mit passendem Stil.
fn selectable_line(selected: bool, text: String, theme: style::Theme) -> Line<'static> {
    let prefix = if selected { "› " } else { "  " };
    let line_style = if selected {
        style::selected_style(theme)
    } else {
        Style::default()
    };
    Line::from(vec![Span::styled(format!("{prefix}{text}"), line_style)])
}

/// Maskiert einen getippten Key für die Anzeige.
fn mask(key: &str) -> String {
    "*".repeat(key.chars().count())
}

/// Bricht `text` auf Leerzeichen-Grenzen in Zeilen von höchstens `width`
/// Zeichen um, ohne einzelne Wörter zu verändern.
///
/// # Description
/// Reine Anzeigehilfe für lange Hinweiszeilen in den nicht umbrechenden
/// Listen-Phasen (siehe [`selected_line_index`]). Der Ursprungstext bleibt
/// wortgleich erhalten — es werden nur Zeilenumbrüche eingefügt.
fn wrap_warning(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let extra = usize::from(!current.is_empty());
        if !current.is_empty() && current.chars().count() + extra + word.chars().count() > width {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use crossterm::event::KeyModifiers;

    /// Erzeugt einen `Press`-Tastenanschlag ohne Modifier.
    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn foundry_endpoint_api_and_deployment_are_explicit() -> TestResult {
        for (choice, api, suffix, header) in [
            (0, "openai-chat", "/openai/v1", "api-key"),
            (1, "openai-responses", "/openai/v1", "api-key"),
            (2, "anthropic-messages", "/anthropic", "x-api-key"),
            (5, "anthropic-messages", "/anthropic", "bearer"),
        ] {
            let mut provider = groq_provider();
            provider.id = "foundry".into();
            provider.base_url = "https://<resource>.services.ai.azure.com".into();
            let mut app = SetupApp::new(vec![provider]);
            app.on_key(press(KeyCode::Enter));
            assert_eq!(app.stage, SetupStage::Endpoint);
            app.on_key(press(KeyCode::Enter));
            assert_eq!(app.stage, SetupStage::Endpoint);
            app.on_paste("https://resource.services.ai.azure.com/".into());
            app.on_key(press(KeyCode::Enter));
            assert_eq!(app.stage, SetupStage::Api);
            app.selected = choice;
            app.on_key(press(KeyCode::Enter));
            app.on_paste("env:FOUNDRY_TEST_KEY".into());
            app.on_key(press(KeyCode::Enter));
            assert_eq!(app.stage, SetupStage::Model);
            app.on_key(press(KeyCode::Enter));
            assert_eq!(app.stage, SetupStage::Model);
            app.on_paste("production-deployment".into());
            app.on_key(press(KeyCode::Enter));
            let outcome = app.outcome().ok_or(TestError::Missing("outcome present"))?;
            assert_eq!(outcome.api, api);
            assert_eq!(
                outcome.base_url,
                format!("https://resource.services.ai.azure.com{suffix}")
            );
            assert_eq!(outcome.model, "production-deployment");
            assert_eq!(outcome.auth_header.as_deref(), Some(header));
        }
        Ok(())
    }

    /// Provider mit API-Key-Auth und zwei Modellen (ohne eingebettete Quellen).
    fn groq_provider() -> ProviderSpec {
        ProviderSpec {
            id: "groq".to_owned(),
            name: "Groq".to_owned(),
            base_url: "https://api.groq.com/openai/v1".to_owned(),
            api: ProviderApi::OpenAiChat,
            auth: vec![AuthMethod::ApiKey {
                env_vars: vec!["GROQ_API_KEY".to_owned()],
            }],
            default_model: Some("llama-3.1-8b".to_owned()),
            featured: false,
            models: vec!["llama-3.1-8b".to_owned(), "llama-3.3-70b".to_owned()],
        }
    }

    /// Lokaler Provider ohne Geheimnis.
    fn ollama_provider() -> ProviderSpec {
        ProviderSpec {
            id: "ollama".to_owned(),
            name: "Ollama".to_owned(),
            base_url: "http://localhost:11434".to_owned(),
            api: ProviderApi::Ollama,
            auth: vec![AuthMethod::LocalBaseUrl],
            default_model: Some("llama3".to_owned()),
            featured: false,
            models: Vec::new(),
        }
    }

    #[test]
    fn test_api_str_all_variants() {
        assert_eq!(api_str(ProviderApi::OpenAiResponses), "openai-responses");
        assert_eq!(api_str(ProviderApi::OpenAiChat), "openai-chat");
        assert_eq!(
            api_str(ProviderApi::AnthropicMessages),
            "anthropic-messages"
        );
        assert_eq!(api_str(ProviderApi::Ollama), "ollama");
    }

    #[test]
    fn test_apikey_flow_produces_outcome() -> TestResult {
        let mut app = SetupApp::new(vec![groq_provider(), ollama_provider()]);
        assert_eq!(app.stage(), SetupStage::Provider);

        // Provider 0 (groq) bestätigen -> Auth.
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Enter)); // Confirm endpoint.
        assert_eq!(app.stage(), SetupStage::Auth);
        assert_eq!(app.auth_options, vec![AuthOption::ApiKey]);

        // Key tippen — erstes Zeichen aktiviert Editing-Modus.
        for character in "gsk".chars() {
            app.on_key(press(KeyCode::Char(character)));
        }
        app.on_key(press(KeyCode::Enter));
        assert_eq!(app.stage(), SetupStage::Model);

        // Zweites Modell wählen.
        app.on_key(press(KeyCode::Down));
        app.on_key(press(KeyCode::Enter));
        assert_eq!(app.stage(), SetupStage::Done);

        let outcome = app.outcome().ok_or(TestError::Missing("outcome present"))?;
        assert_eq!(outcome.provider_id, "groq");
        assert_eq!(outcome.base_url, "https://api.groq.com/openai/v1");
        assert_eq!(outcome.api, "openai-chat");
        assert_eq!(outcome.model, "llama-3.3-70b");
        assert_eq!(outcome.secret_ref.as_deref(), Some("gsk"));
        Ok(())
    }

    #[test]
    fn test_filter_narrows_provider_list() {
        let mut app = SetupApp::new(vec![groq_provider(), ollama_provider()]);
        for character in "oll".chars() {
            app.on_key(press(KeyCode::Char(character)));
        }
        assert_eq!(app.filtered_indices(), vec![1]);
        // selected geklemmt auf einziges Ergebnis.
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Enter)); // Confirm endpoint.
        assert_eq!(app.stage(), SetupStage::Auth);
        assert_eq!(
            app.chosen_provider.as_ref().map(|p| p.id.as_str()),
            Some("ollama")
        );
    }

    #[test]
    fn test_local_base_url_flow_has_no_secret() -> TestResult {
        let mut app = SetupApp::new(vec![ollama_provider()]);
        app.on_key(press(KeyCode::Enter)); // Provider -> Auth
        app.on_key(press(KeyCode::Enter)); // Confirm endpoint.
        assert_eq!(app.auth_options, vec![AuthOption::LocalBaseUrl]);
        app.on_key(press(KeyCode::Enter)); // Auth -> Model
        assert_eq!(app.stage(), SetupStage::Model);
        // default_model als einzige Option.
        assert_eq!(app.model_options, vec!["llama3".to_owned()]);
        app.on_key(press(KeyCode::Enter)); // Model -> Done

        let outcome = app.outcome().ok_or(TestError::Missing("outcome present"))?;
        assert_eq!(outcome.provider_id, "ollama");
        assert_eq!(outcome.api, "ollama");
        assert_eq!(outcome.model, "llama3");
        assert_eq!(outcome.secret_ref, None);
        Ok(())
    }

    /// Nativer Anthropic-Provider (Provider-Id `"anthropic"`,
    /// `anthropic-messages`) für die OAuth-Warnungs-Tests.
    fn anthropic_provider() -> ProviderSpec {
        ProviderSpec {
            id: "anthropic".to_owned(),
            name: "Anthropic".to_owned(),
            base_url: "https://api.anthropic.com/v1".to_owned(),
            api: ProviderApi::AnthropicMessages,
            auth: vec![
                AuthMethod::ApiKey {
                    env_vars: vec!["ANTHROPIC_API_KEY".to_owned()],
                },
                AuthMethod::LocalImport {
                    sources: vec!["claude-setup-token".to_owned(), "claude-cli".to_owned()],
                },
            ],
            default_model: Some("claude-opus-4-8".to_owned()),
            featured: true,
            models: vec!["claude-opus-4-8".to_owned()],
        }
    }

    /// Setzt deterministische Auth-Optionen, unabhängig von lokal erkannten
    /// Credentials (z. B. `~/.claude/.credentials.json` auf der Build-Maschine).
    fn force_anthropic_options(app: &mut SetupApp) {
        app.auth_options = vec![
            AuthOption::ApiKey,
            AuthOption::OAuth {
                label: "OAuth/Setup-Token: claude-setup-token".to_owned(),
                secret_ref: "env:CLAUDE_CODE_OAUTH_TOKEN".to_owned(),
            },
        ];
        app.selected = 0;
    }

    #[test]
    fn anthropic_oauth_selection_shows_warning_apikey_clears_it() {
        let mut app = SetupApp::new(vec![anthropic_provider()]);
        app.on_key(press(KeyCode::Enter)); // Provider -> Endpoint
        app.on_key(press(KeyCode::Enter)); // Endpoint -> Auth
        assert_eq!(app.stage(), SetupStage::Auth);
        force_anthropic_options(&mut app);
        // Erste Option ist der deklarierte API-Key (x-api-key) — keine Warnung.
        assert_eq!(app.selected, 0);
        assert!(matches!(app.auth_options[0], AuthOption::ApiKey));
        assert!(app.oauth_warning().is_none());

        // Weiter zur OAuth-/Setup-Token-Option — Warnung erscheint, wortgleich.
        app.on_key(press(KeyCode::Down));
        assert!(matches!(
            app.auth_options[app.selected],
            AuthOption::OAuth { .. }
        ));
        assert_eq!(app.oauth_warning(), Some(ANTHROPIC_OAUTH_WARNING));

        // Zurück zur API-Key-Option — Warnung verschwindet wieder.
        app.on_key(press(KeyCode::Up));
        assert!(app.oauth_warning().is_none());
    }

    #[test]
    fn anthropic_pasted_setup_token_shows_warning_on_apikey_option() {
        let mut app = SetupApp::new(vec![anthropic_provider()]);
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Enter));
        force_anthropic_options(&mut app);
        assert_eq!(app.selected, 0);
        assert!(app.oauth_warning().is_none());

        // Ein eingefügtes `sk-ant-oat...`-Token in der API-Key-Option löst die
        // Warnung ebenfalls aus.
        app.on_paste("sk-ant-oat01-fake-token-for-test".into());
        assert!(app.oauth_warning().is_some());
    }

    #[test]
    fn anthropic_plain_api_key_never_warns() {
        let mut app = SetupApp::new(vec![anthropic_provider()]);
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Enter));
        force_anthropic_options(&mut app);
        app.on_paste("sk-ant-api03-not-an-oauth-token".into());
        assert!(app.oauth_warning().is_none());
    }

    #[test]
    fn non_anthropic_provider_never_shows_oauth_warning() {
        let mut app = SetupApp::new(vec![groq_provider()]);
        app.on_key(press(KeyCode::Enter));
        app.on_key(press(KeyCode::Enter));
        assert_eq!(app.stage(), SetupStage::Auth);
        assert!(app.oauth_warning().is_none());
    }

    #[test]
    fn wrap_warning_preserves_words_and_splits_on_width() {
        let wrapped = wrap_warning("eins zwei drei vier fuenf", 12);
        assert_eq!(wrapped.join(" "), "eins zwei drei vier fuenf");
        assert!(wrapped.iter().all(|line| line.chars().count() <= 12 + 5));
    }

    #[test]
    fn test_provider_navigation_down_and_up() {
        let mut app = SetupApp::new(vec![groq_provider(), ollama_provider()]);
        app.on_key(press(KeyCode::Down));
        assert_eq!(app.selected, 1);
        app.on_key(press(KeyCode::Down)); // geklemmt am Ende
        assert_eq!(app.selected, 1);
        app.on_key(press(KeyCode::Up));
        assert_eq!(app.selected, 0);
        app.on_key(press(KeyCode::Up)); // geklemmt am Anfang
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn test_no_auth_options_skips_to_model() -> TestResult {
        let provider = ProviderSpec {
            id: "bare".to_owned(),
            name: "Bare".to_owned(),
            base_url: "https://example.test".to_owned(),
            api: ProviderApi::OpenAiChat,
            auth: Vec::new(),
            default_model: Some("m1".to_owned()),
            featured: false,
            models: Vec::new(),
        };
        let mut app = SetupApp::new(vec![provider]);
        app.on_key(press(KeyCode::Enter)); // Provider -> (kein Auth) -> Model
        app.on_key(press(KeyCode::Enter)); // Confirm endpoint.
        assert_eq!(app.stage(), SetupStage::Model);
        app.on_key(press(KeyCode::Enter)); // Model -> Done
        let outcome = app.outcome().ok_or(TestError::Missing("outcome present"))?;
        assert_eq!(outcome.model, "m1");
        assert_eq!(outcome.secret_ref, None);
        Ok(())
    }

    #[test]
    fn test_apikey_backspace_edits_buffer() {
        let mut app = SetupApp::new(vec![groq_provider()]);
        app.on_key(press(KeyCode::Enter)); // -> Auth (ApiKey selected)
        app.on_key(press(KeyCode::Enter)); // Confirm endpoint.
        for character in "abc".chars() {
            app.on_key(press(KeyCode::Char(character)));
        }
        app.on_key(press(KeyCode::Backspace));
        assert_eq!(app.api_key, "ab");
    }

    #[test]
    fn test_done_stage_ignores_keys() {
        let mut app = SetupApp::new(vec![ollama_provider()]);
        app.on_key(press(KeyCode::Enter)); // -> Auth
        app.on_key(press(KeyCode::Enter)); // Confirm endpoint.
        app.on_key(press(KeyCode::Enter)); // -> Model
        app.on_key(press(KeyCode::Enter)); // -> Done
        assert_eq!(app.stage(), SetupStage::Done);
        app.on_key(press(KeyCode::Down));
        app.on_key(press(KeyCode::Enter));
        assert_eq!(app.stage(), SetupStage::Done);
    }

    // --- Neue Tests für SLICE 1 ---

    /// Paste in der Auth-Phase füllt den Eingabepuffer und aktiviert den
    /// Editing-Modus. Bug 1 (Bracketed-Paste) + Bug 2 (Eingabefeld).
    #[test]
    fn test_paste_fills_api_key() -> TestResult {
        let mut app = SetupApp::new(vec![groq_provider()]);
        app.on_key(press(KeyCode::Enter)); // Provider -> Auth
        app.on_key(press(KeyCode::Enter)); // Confirm endpoint.
        assert_eq!(app.stage(), SetupStage::Auth);
        assert!(!app.is_auth_editing(), "vor Paste: nicht im Editing-Modus");

        app.on_paste("sk-my-api-key".to_owned());

        assert!(app.is_auth_editing(), "nach Paste: Editing-Modus aktiv");
        assert_eq!(app.api_key, "sk-my-api-key");

        // Bestätigen und vollständigen Flow prüfen.
        app.on_key(press(KeyCode::Enter)); // confirm_auth -> Model
        assert_eq!(app.stage(), SetupStage::Model);
        app.on_key(press(KeyCode::Enter)); // Model -> Done
        let outcome = app.outcome().ok_or(TestError::Missing("outcome present"))?;
        assert_eq!(outcome.secret_ref.as_deref(), Some("sk-my-api-key"));
        Ok(())
    }

    /// Paste wird getrimmt — führende/nachgestellte Whitespace werden entfernt.
    #[test]
    fn test_paste_trims_whitespace() {
        let mut app = SetupApp::new(vec![groq_provider()]);
        app.on_key(press(KeyCode::Enter)); // -> Auth
        app.on_key(press(KeyCode::Enter)); // Confirm endpoint.
        app.on_paste("  sk-trimmed-key  \n".to_owned());
        assert_eq!(app.api_key, "sk-trimmed-key");
    }

    /// Zifferntaste `'1'` bei ApiKey-Option: sofortiger Übergang in Editing-Modus.
    /// Bug 2 (Sofort-Routing). Contract-Quelle: Redesign-Spec SLICE 1, Punkt c.
    #[test]
    fn test_digit_selects_option() {
        let mut app = SetupApp::new(vec![groq_provider()]);
        app.on_key(press(KeyCode::Enter)); // Provider -> Auth
        app.on_key(press(KeyCode::Enter)); // Confirm endpoint.
        assert_eq!(app.stage(), SetupStage::Auth);
        assert_eq!(app.auth_options, vec![AuthOption::ApiKey]);
        assert!(!app.is_auth_editing());

        // '1' wählt Index 0 (ApiKey) und startet den Editing-Modus direkt.
        app.on_key(press(KeyCode::Char('1')));

        assert_eq!(app.selected, 0);
        assert!(
            app.is_auth_editing(),
            "Digit '1' auf ApiKey startet Editing-Modus"
        );
    }

    /// Zifferntaste auf LocalBaseUrl-Option: sofortige Bestätigung ohne Editing.
    #[test]
    fn test_digit_selects_non_secret_option_confirms_immediately() {
        let mut app = SetupApp::new(vec![ollama_provider()]);
        app.on_key(press(KeyCode::Enter)); // -> Auth (LocalBaseUrl only)
        app.on_key(press(KeyCode::Enter)); // Confirm endpoint.
        assert_eq!(app.auth_options, vec![AuthOption::LocalBaseUrl]);

        // '1' wählt LocalBaseUrl und bestätigt sofort (kein Secret nötig).
        app.on_key(press(KeyCode::Char('1')));

        // confirm_auth() → begin_model() wurde aufgerufen.
        assert_eq!(app.stage(), SetupStage::Model);
        assert!(!app.is_auth_editing());
    }

    /// OAuth-Option: Paste setzt den Token in `api_key`; confirm_auth() übernimmt
    /// ihn als `secret_ref`. Bug 3 (OAuth einfügbar).
    /// Contract-Quelle: Redesign-Spec SLICE 1, Punkte d, e, f.
    #[test]
    fn test_oauth_paste_sets_raw_token() -> TestResult {
        let mut app = SetupApp::new(vec![groq_provider()]);
        // Auth-Phase manuell mit OAuth-Option aufsetzen (private Felder sind im
        // selben Modul zugänglich).
        app.stage = SetupStage::Auth;
        app.auth_options = vec![AuthOption::OAuth {
            label: "OAuth/Setup-Token: test-source (via `harw auth login`)".to_owned(),
            secret_ref: "env:TEST_OAUTH_TOKEN".to_owned(),
        }];
        app.selected = 0;
        // `finish()` benötigt einen gewählten Provider, um das Outcome zu bauen.
        app.chosen_provider = Some(groq_provider());

        // Paste des Tokens.
        app.on_paste("my-oauth-token-xyz".to_owned());

        assert!(app.is_auth_editing(), "nach Paste: Editing-Modus aktiv");
        assert_eq!(app.api_key, "my-oauth-token-xyz");

        // Bestätigen → Raw-Token wird als secret_ref übernommen (nicht der Env-Ref).
        app.on_key(press(KeyCode::Enter));
        assert_eq!(app.stage(), SetupStage::Model);
        app.on_key(press(KeyCode::Enter)); // Model -> Done
        let outcome = app.outcome().ok_or(TestError::Missing("outcome present"))?;
        assert_eq!(
            outcome.secret_ref.as_deref(),
            Some("my-oauth-token-xyz"),
            "Raw-Token muss als secret_ref statt des Katalog-Ref übernommen werden"
        );
        Ok(())
    }

    /// OAuth-Option ohne eingefügten Token: `secret_ref` aus dem Katalog als Fallback.
    #[test]
    fn test_oauth_confirm_without_token_uses_catalog_ref() -> TestResult {
        let mut app = SetupApp::new(vec![groq_provider()]);
        app.stage = SetupStage::Auth;
        app.auth_options = vec![AuthOption::OAuth {
            label: "OAuth/Setup-Token: fallback".to_owned(),
            secret_ref: "env:FALLBACK_TOKEN".to_owned(),
        }];
        app.selected = 0;
        // `finish()` benötigt einen gewählten Provider, um das Outcome zu bauen.
        app.chosen_provider = Some(groq_provider());

        // Kein Paste — direkt bestätigen.
        app.on_key(press(KeyCode::Enter)); // Enter bei leerem Puffer → auth_editing=true
        assert!(app.is_auth_editing());
        app.on_key(press(KeyCode::Enter)); // zweites Enter mit leerem Puffer → confirm_auth()

        assert_eq!(app.stage(), SetupStage::Model);
        app.on_key(press(KeyCode::Enter)); // -> Done
        let outcome = app.outcome().ok_or(TestError::Missing("outcome present"))?;
        assert_eq!(
            outcome.secret_ref.as_deref(),
            Some("env:FALLBACK_TOKEN"),
            "Katalog-Ref muss als Fallback dienen wenn kein Token eingegeben"
        );
        Ok(())
    }

    /// Esc im Editing-Modus leert den Puffer und kehrt in den Navigations-Modus
    /// zurück, ohne den Assistenten zu beenden.
    #[test]
    fn test_esc_in_editing_mode_cancels_input() {
        let mut app = SetupApp::new(vec![groq_provider()]);
        app.on_key(press(KeyCode::Enter)); // -> Auth
        app.on_key(press(KeyCode::Enter)); // Confirm endpoint.

        // Editing-Modus starten durch Tippen.
        app.on_key(press(KeyCode::Char('a')));
        assert!(app.is_auth_editing());
        assert_eq!(app.api_key, "a");

        // Esc soll nur Eingabe leeren, nicht den Assistenten beenden.
        // (setup_loop würde cancel_auth_editing aufrufen; hier direkt testen)
        app.cancel_auth_editing();
        assert!(!app.is_auth_editing());
        assert!(app.api_key.is_empty());
        assert_eq!(app.stage(), SetupStage::Auth);
    }

    /// Up/Down im Editing-Modus beendet den Editing-Modus ohne Bestätigung.
    #[test]
    fn test_navigation_exits_editing_mode() {
        let mut app = SetupApp::new(vec![groq_provider()]);
        app.on_key(press(KeyCode::Enter)); // -> Auth
        app.on_key(press(KeyCode::Enter)); // Confirm endpoint.
        app.on_key(press(KeyCode::Char('x'))); // -> Editing
        assert!(app.is_auth_editing());

        app.on_key(press(KeyCode::Up));
        assert!(!app.is_auth_editing(), "Up verlässt den Editing-Modus");
        // api_key bleibt erhalten (kein cancel, nur mode-switch).
        assert_eq!(app.api_key, "x");
    }

    /// Prüft, dass `SetupApp` beim Konstruieren ein Theme erhält.
    /// In Test-Umgebungen ohne `COLORFGBG` muss es den Dark-Fallback wählen.
    /// (SLICE 8)
    #[test]
    fn test_setupapp_has_dark_theme_by_default() {
        let app = SetupApp::new(vec![]);
        assert!(
            !style::is_light(app.theme),
            "Standard-Theme ohne COLORFGBG muss Dark sein"
        );
    }

    // --- follow_scroll ---

    #[test]
    fn test_follow_scroll_line_inside_viewport_keeps_current_offset() {
        // line 5 liegt im Fenster [current=2, current+viewport=12) -> Offset bleibt.
        assert_eq!(follow_scroll(2, 5, 10), 2);
    }

    #[test]
    fn test_follow_scroll_line_below_viewport_scrolls_minimally() {
        // Reproduziert den ursprünglichen Bug: viele Modelle (z. B. OpenRouter)
        // schieben die Markierung unter das Sichtfenster.
        assert_eq!(follow_scroll(0, 30, 10), 21);
    }

    #[test]
    fn test_follow_scroll_line_above_offset_scrolls_up_to_line() {
        assert_eq!(follow_scroll(10, 3, 5), 3);
    }

    #[test]
    fn test_follow_scroll_saturates_on_huge_values() {
        // offset = 1_000_000 + 1 - 1 = 1_000_000, was u16 weit übersteigt ->
        // Sättigung auf u16::MAX statt Panic/Wraparound. `usize::MAX` selbst
        // würde bereits bei `line + 1` überlaufen und ist daher ungeeignet.
        assert_eq!(follow_scroll(0, 1_000_000, 1), u16::MAX);
    }

    // --- selected_line_index ---

    #[test]
    fn test_selected_line_index_provider_stage_offsets_by_one() {
        let mut app = SetupApp::new(vec![groq_provider(), ollama_provider()]);
        app.selected = 1;
        assert_eq!(selected_line_index(&app), Some(2));
    }

    #[test]
    fn test_selected_line_index_api_stage_no_offset() {
        let mut app = SetupApp::new(vec![groq_provider()]);
        app.stage = SetupStage::Api;
        app.selected = 4;
        assert_eq!(selected_line_index(&app), Some(4));
    }

    #[test]
    fn test_selected_line_index_auth_stage_offsets_by_one() {
        let mut app = SetupApp::new(vec![groq_provider()]);
        app.stage = SetupStage::Auth;
        app.auth_options = vec![AuthOption::ApiKey, AuthOption::Custom];
        app.selected = 1;
        assert_eq!(selected_line_index(&app), Some(2));
    }

    #[test]
    fn test_selected_line_index_model_stage_offsets_by_three() {
        let mut app = SetupApp::new(vec![groq_provider()]);
        app.stage = SetupStage::Model;
        app.model_options = vec!["a".to_owned(), "b".to_owned()];
        app.selected = 1;
        assert_eq!(selected_line_index(&app), Some(4));
    }

    #[test]
    fn test_selected_line_index_endpoint_stage_is_none() {
        let mut app = SetupApp::new(vec![groq_provider()]);
        app.stage = SetupStage::Endpoint;
        assert_eq!(selected_line_index(&app), None);
    }

    #[test]
    fn test_selected_line_index_done_stage_is_none() {
        let mut app = SetupApp::new(vec![groq_provider()]);
        app.stage = SetupStage::Done;
        assert_eq!(selected_line_index(&app), None);
    }
}

//! Telegram-Befehle des Gateways: Befehlsmenü je Scope und die Verarbeitung
//! der geschlossenen Befehlsgrammatik (`/task`, `/request`, `/review`,
//! `/approve`, `/deny`, `/cancel`, `/workspace`, `/new`, `/status`, `/help`,
//! `/start`).
//!
//! # Sicherheitsgrenze
//! Der [`TelegramCommandHandler`] sieht nur Ereignisse, die die Admission
//! von `TelegramChannel` bereits zugelassen hat. Er wertet ausschließlich die
//! von `harw_channel_telegram_transport::parse_command` extrahierten,
//! grammatisch geprüften Argumente aus; Nachrichtentexte werden nie
//! protokolliert. Handelnder ist immer der tatsächliche Absender
//! (`SenderRef::id`), nie eine Gruppen-`PeerId`; Steuerbefehle auf
//! Arbeitsaufträge laufen über die `*_as`-Methoden des
//! [`WorkRequestStore`] (gleiches Binding, gleicher Tenant, Anfragender oder
//! Admin).
//!
//! Das Befehlsmenü ([`telegram_menu_plan`]) ist reine Anzeige; die
//! Autorisierung liegt in Admission und Befehlsverarbeitung.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use jiff::Timestamp;

use harw_channel::{
    ApprovalAction, ApprovalPrompt, InboundEvent, OutboundContent, PeerId, SessionKey,
};
use harw_channel_telegram::{
    ChatStateStore, TelegramChannelError, WorkRequestActor, WorkRequestRecord, WorkRequestStore,
};
use harw_channel_telegram_transport::{
    BotCommand, BotCommandScope, TelegramClient, TelegramCommand, TelegramOutbound,
    is_command_like, parse_command,
};
use harw_config::TelegramChannelToml;
use harw_types::WorkId;

/// Präfix der `request_id` von Freigabe-Schaltflächen für Arbeitsaufträge
/// (`"work:" + WorkId`), ausgewertet vom Callback-Worker.
const WORK_APPROVAL_PREFIX: &str = super::telegram_callbacks::APPROVAL_KIND_WORK;

/// Höchstzahl der unter `/status` gelisteten Arbeitsaufträge.
const STATUS_WORK_LIMIT: usize = 5;

/// Ein vom Gateway verarbeiteter Befehl: Name (ohne `/`), Syntax für die
/// Hilfe und Menübeschreibung (3–256 Zeichen, Bot-API-Grenzen).
struct CommandSpec {
    name: &'static str,
    usage: &'static str,
    description: &'static str,
}

/// Alle Befehle, die der [`TelegramCommandHandler`] für admittierte Peers
/// ausführt, in Menü- und Hilfereihenfolge. `/pair` fehlt bewusst: es wird
/// ausschließlich von der Admission-Grenze ausgewertet.
const HANDLED_COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "task",
        usage: "/task <rolle> <aufgabe>",
        description: "Arbeitsauftrag im aktuellen Arbeitsbereich: /task <rolle> <aufgabe>",
    },
    CommandSpec {
        name: "request",
        usage: "/request <workspace> <rolle> <aufgabe>",
        description: "Arbeitsauftrag anfragen: /request <workspace> <rolle> <aufgabe>",
    },
    CommandSpec {
        name: "review",
        usage: "/review <work-id>",
        description: "Arbeitsauftrag prüfen: /review <work-id>",
    },
    CommandSpec {
        name: "approve",
        usage: "/approve <work-id>",
        description: "Arbeitsauftrag freigeben: /approve <work-id>",
    },
    CommandSpec {
        name: "deny",
        usage: "/deny <work-id>",
        description: "Arbeitsauftrag ablehnen: /deny <work-id>",
    },
    CommandSpec {
        name: "cancel",
        usage: "/cancel <work-id>",
        description: "Arbeitsauftrag abbrechen: /cancel <work-id>",
    },
    CommandSpec {
        name: "workspace",
        usage: "/workspace [alias]",
        description: "Arbeitsbereich anzeigen oder wechseln: /workspace [alias]",
    },
    CommandSpec {
        name: "new",
        usage: "/new",
        description: "Neue Unterhaltung beginnen",
    },
    CommandSpec {
        name: "status",
        usage: "/status",
        description: "Status dieses Chats und offener Arbeitsaufträge",
    },
    CommandSpec {
        name: "help",
        usage: "/help",
        description: "Hilfe zu den verfügbaren Befehlen",
    },
    CommandSpec {
        name: "start",
        usage: "/start",
        description: "Begrüßung und Kurzhilfe",
    },
];

/// Menüeintrag für `/pair` in Privatchats (nur Anzeige; ausgewertet wird der
/// Befehl von der Admission-Grenze).
const PAIR_COMMAND: CommandSpec = CommandSpec {
    name: "pair",
    usage: "/pair <code>",
    description: "Chat mit einem Kopplungscode verbinden: /pair <code>",
};

fn bot_command(spec: &CommandSpec) -> BotCommand {
    BotCommand {
        command: spec.name.to_owned(),
        description: spec.description.to_owned(),
    }
}

fn commands_named(names: &[&str]) -> Vec<BotCommand> {
    HANDLED_COMMANDS
        .iter()
        .filter(|spec| names.contains(&spec.name))
        .map(bot_command)
        .collect()
}

/// Die Befehle, die dieser Gateway für **jeden** admittierten Peer
/// tatsächlich verarbeitet ([`TelegramCommandHandler::handle`]); ohne
/// `/pair`.
pub(super) fn telegram_handled_commands() -> Vec<BotCommand> {
    HANDLED_COMMANDS.iter().map(bot_command).collect()
}

/// Leitet den Menüplan einer Bindung aus `[channel.telegram.commands].menu_source` ab.
///
/// # Description
/// - `"policy_visible"` (Vorgabe): Telegram wählt je Chat den
///   spezifischsten Scope. Allgemeine Scopes zeigen nur, was jeder sieht:
///   `Default` und `AllGroupChats` → `help`, `start`; `AllPrivateChats` →
///   `help`, `start`, `pair`. Das volle Menü ([`telegram_handled_commands`])
///   erhält `Chat{id}` je gepinnter Identität (DM-Chat-ID = User-ID) und je
///   erlaubter Gruppe; bei konfigurierten Admin-Identitäten zusätzlich
///   `ChatAdministrators{id}` je erlaubter Gruppe, damit ein veraltetes
///   Admin-Menü den Gruppen-Scope nicht überdeckt.
/// - `"static"`: das volle Menü im `Default`-Scope.
/// - `"none"`: leerer Plan (nichts wird veröffentlicht).
///
/// # Errors
/// Unbekannte `menu_source` oder eine nicht numerische Gruppen-Chat-ID;
/// die Meldung enthält keine Geheimnisse.
pub(super) fn telegram_menu_plan(
    binding: &TelegramChannelToml,
) -> Result<Vec<(BotCommandScope, Vec<BotCommand>)>, String> {
    match binding.commands.menu_source.trim() {
        "policy_visible" => {
            let basic = commands_named(&["help", "start"]);
            let mut private = basic.clone();
            private.push(bot_command(&PAIR_COMMAND));
            let full = telegram_handled_commands();
            let mut plan = vec![
                (BotCommandScope::Default, basic.clone()),
                (BotCommandScope::AllGroupChats, basic),
                (BotCommandScope::AllPrivateChats, private),
            ];
            let pinned: BTreeSet<i64> =
                binding.security.pinned_identities.iter().copied().collect();
            for chat_id in pinned {
                plan.push((BotCommandScope::Chat { chat_id }, full.clone()));
            }
            let mut groups = BTreeSet::new();
            for raw in &binding.groups.allowed_chats {
                let chat_id = raw.trim().parse::<i64>().map_err(|_| {
                    format!("groups.allowed_chats enthält keine numerische Chat-ID: {raw:?}")
                })?;
                groups.insert(chat_id);
            }
            let admin_groups = !binding.security.admin_identities.is_empty();
            for chat_id in groups {
                plan.push((BotCommandScope::Chat { chat_id }, full.clone()));
                if admin_groups {
                    plan.push((
                        BotCommandScope::ChatAdministrators { chat_id },
                        full.clone(),
                    ));
                }
            }
            Ok(plan)
        }
        "static" => Ok(vec![(
            BotCommandScope::Default,
            telegram_handled_commands(),
        )]),
        "none" => Ok(Vec::new()),
        other => Err(format!(
            "unbekannte commands.menu_source {other:?} (erwartet policy_visible, static oder none)"
        )),
    }
}

/// Veröffentlicht den Menüplan einer Bindung (best effort: Fehler werden je
/// Scope geloggt, schließen die Bindung aber nicht — das Menü ist reine
/// Anzeige).
pub(super) async fn publish_telegram_command_menu(
    binding: &TelegramChannelToml,
    client: &TelegramClient,
) {
    let plan = match telegram_menu_plan(binding) {
        Ok(plan) => plan,
        Err(reason) => {
            tracing::warn!(binding = %binding.id, reason = %reason, "Telegram command menu not published");
            return;
        }
    };
    if plan.is_empty() {
        tracing::debug!(binding = %binding.id, "Telegram command menu publication disabled by config");
        return;
    }
    for (scope, commands) in &plan {
        if let Err(error) = client.set_my_commands_scoped(commands, scope).await {
            tracing::warn!(binding = %binding.id, ?scope, error = %error, "Telegram command menu scope could not be published");
        }
    }
}

/// Umgang mit unbekannten (oder syntaktisch unvollständigen) Befehlen,
/// `[channel.telegram.commands].unknown_command_fallback`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UnknownCommandFallback {
    /// Mit der Befehlsübersicht antworten.
    ReplyHelp,
    /// Als normale Nachricht an den Assistenten weiterreichen.
    PassThrough,
    /// Stillschweigend verwerfen.
    Ignore,
}

impl UnknownCommandFallback {
    /// Liest den Konfigurationswert (`"reply_help"`, `"pass_through"`,
    /// `"ignore"`).
    ///
    /// # Errors
    /// Jeder andere Wert.
    pub(super) fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "reply_help" => Ok(Self::ReplyHelp),
            "pass_through" => Ok(Self::PassThrough),
            "ignore" => Ok(Self::Ignore),
            other => Err(format!(
                "unbekannter commands.unknown_command_fallback {other:?} (erwartet reply_help, pass_through oder ignore)"
            )),
        }
    }
}

/// Ergebnis von [`TelegramCommandHandler::handle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandDisposition {
    /// Als Befehl verarbeitet (oder bewusst verworfen); nicht an das Modell.
    Handled,
    /// Kein Befehl: normale Nachricht für die Modell-Sitzung.
    NotACommand,
}

/// Verarbeitet die geschlossenen Telegram-Befehle eines admittierten
/// Ereignisses.
pub(super) struct TelegramCommandHandler {
    pub outbound: Arc<dyn TelegramOutbound>,
    /// Durabler Lebenszyklus-Store der Arbeitsaufträge dieses Bindings.
    pub work_requests: Arc<WorkRequestStore>,
    /// Autoritative Workspace-Auflösung (aus `binding.workspaces`).
    pub workspaces: Arc<harw_authority::WorkspaceRegistry>,
    /// Workspace-Wahl und Sitzungsgeneration je Chat/Thema.
    pub chat_state: Arc<ChatStateStore>,
    /// Telegram-User-IDs mit Admin-Rechten (`security.admin_identities`).
    pub admin_sender_ids: HashSet<String>,
    pub unknown_command_fallback: UnknownCommandFallback,
    /// Alias des Workspaces mit `default = true`, falls konfiguriert; gilt
    /// für `/task`, solange der Chat keinen eigenen Arbeitsbereich gewählt hat.
    pub default_workspace_alias: Option<String>,
}

/// Antwortziel eines Ereignisses.
struct ReplyTarget {
    chat_id: i64,
    thread_id: Option<i64>,
}

impl TelegramCommandHandler {
    /// Verarbeitet `text` als Befehl, falls es einer ist.
    ///
    /// # Returns
    /// - [`CommandDisposition::Handled`] für jeden erkannten Befehl (auch bei
    ///   fachlichem Fehler — der Nutzer erhält eine Antwort), für bekannte
    ///   Befehle mit ungültigen Argumenten (Antwort mit der Syntax), für
    ///   `/pair` (wird stillschweigend verworfen) und für unbekannte Befehle
    ///   bei `ReplyHelp`/`Ignore`.
    /// - [`CommandDisposition::NotACommand`] für normalen Text und für
    ///   unbekannte Befehle bei `PassThrough`.
    pub(super) fn handle(
        &self,
        key: &SessionKey,
        event: &InboundEvent,
        text: &str,
    ) -> CommandDisposition {
        let Some(command) = parse_command(text) else {
            if !is_command_like(text) {
                return CommandDisposition::NotACommand;
            }
            return self.handle_unparsed_command(key, event, text);
        };
        let Some(target) = reply_target(event) else {
            tracing::warn!(channel = %key.channel, peer = %key.peer, "Telegram peer is not a numeric chat id");
            return CommandDisposition::Handled;
        };
        if matches!(command, TelegramCommand::Pair) {
            tracing::debug!(channel = %key.channel, peer = %key.peer, "Telegram pairing command consumed outside command handler");
            return CommandDisposition::Handled;
        }
        let Some(actor) = self.actor(key, event) else {
            tracing::warn!(channel = %key.channel, peer = %key.peer, "Telegram command without sender identity dropped");
            return CommandDisposition::Handled;
        };
        let now = Timestamp::now();
        let reply = match command {
            TelegramCommand::Request {
                workspace_alias,
                role,
                task,
            } => {
                self.submit_work(
                    key,
                    event,
                    &actor,
                    &target,
                    &workspace_alias,
                    &role,
                    &task,
                    now,
                );
                return CommandDisposition::Handled;
            }
            TelegramCommand::Task { role, task } => match self.effective_workspace(key) {
                Ok(Some(alias)) => {
                    self.submit_work(key, event, &actor, &target, &alias, &role, &task, now);
                    return CommandDisposition::Handled;
                }
                Ok(None) => "Kein Arbeitsbereich gewählt. Wähle einen mit /workspace <alias> \
                             oder nutze /request <workspace> <rolle> <aufgabe>."
                    .to_owned(),
                Err(reply) => reply,
            },
            TelegramCommand::Review { work_id } => self.lifecycle(
                "review",
                &work_id,
                self.work_requests
                    .review_as(&WorkId::from_str(&work_id), &actor, now),
            ),
            TelegramCommand::Approve { work_id } => self.lifecycle(
                "approve",
                &work_id,
                self.work_requests
                    .approve_as(&WorkId::from_str(&work_id), &actor, now),
            ),
            TelegramCommand::Deny { work_id } => self.lifecycle(
                "deny",
                &work_id,
                self.work_requests
                    .deny_as(&WorkId::from_str(&work_id), &actor, now),
            ),
            TelegramCommand::Cancel { work_id } => self.lifecycle(
                "cancel",
                &work_id,
                self.work_requests
                    .cancel_as(&WorkId::from_str(&work_id), &actor, now),
            ),
            TelegramCommand::Workspace { alias } => self.workspace(key, alias.as_deref()),
            TelegramCommand::New => match self.chat_state.reset_session(key) {
                Ok(_) => "Neue Unterhaltung begonnen. Der bisherige Verlauf wird nicht \
                          mehr verwendet."
                    .to_owned(),
                Err(error) => {
                    tracing::error!(channel = %key.channel, peer = %key.peer, error = %error, "Telegram chat state could not be reset");
                    "Neue Unterhaltung konnte nicht begonnen werden.".to_owned()
                }
            },
            TelegramCommand::Status => self.status(key, &actor),
            TelegramCommand::Help => help_text(),
            TelegramCommand::Start => format!(
                "Hallo! Ich bin der Harwness-Assistent. Schreib mir einfach, oder nutze \
                 einen der Befehle.\n\n{}",
                help_text()
            ),
            TelegramCommand::Pair => return CommandDisposition::Handled,
        };
        self.reply(key, &target, reply);
        CommandDisposition::Handled
    }

    /// Befehlsähnlicher Text, den `parse_command` nicht akzeptiert: ein
    /// bekannter Befehl mit ungültigen Argumenten oder ein unbekannter Befehl.
    fn handle_unparsed_command(
        &self,
        key: &SessionKey,
        event: &InboundEvent,
        text: &str,
    ) -> CommandDisposition {
        let name = command_name(text);
        if name.as_deref() == Some(PAIR_COMMAND.name) {
            return CommandDisposition::Handled;
        }
        let known = name
            .as_deref()
            .and_then(|name| HANDLED_COMMANDS.iter().find(|spec| spec.name == name));
        let reply = match (known, self.unknown_command_fallback) {
            (Some(spec), _) => format!("Ungültige Angaben. Verwendung: {}", spec.usage),
            (None, UnknownCommandFallback::ReplyHelp) => {
                format!("Unbekannter Befehl.\n\n{}", help_text())
            }
            (None, UnknownCommandFallback::PassThrough) => {
                return CommandDisposition::NotACommand;
            }
            (None, UnknownCommandFallback::Ignore) => {
                tracing::debug!(channel = %key.channel, peer = %key.peer, "Telegram unknown command ignored");
                return CommandDisposition::Handled;
            }
        };
        match reply_target(event) {
            Some(target) => self.reply(key, &target, reply),
            None => {
                tracing::warn!(channel = %key.channel, peer = %key.peer, "Telegram peer is not a numeric chat id");
            }
        }
        CommandDisposition::Handled
    }

    /// Der Handelnde: Binding und Tenant aus dem Sitzungsschlüssel, Peer ist
    /// der tatsächliche Absender. Ohne Absender `None` (fail closed).
    fn actor(&self, key: &SessionKey, event: &InboundEvent) -> Option<WorkRequestActor> {
        let sender = event.sender.as_ref()?;
        if sender.id.trim().is_empty() {
            return None;
        }
        Some(WorkRequestActor {
            channel: key.channel.clone(),
            tenant: key.tenant.clone(),
            peer: PeerId::from_str(sender.id.clone()),
            is_admin: self.admin_sender_ids.contains(&sender.id),
        })
    }

    /// Der für `/task` wirksame Arbeitsbereich: Chat-Wahl, sonst Default.
    /// `Err` trägt die Nutzerantwort bei einem Lesefehler.
    fn effective_workspace(&self, key: &SessionKey) -> Result<Option<String>, String> {
        match self.chat_state.get(key) {
            Ok(state) => Ok(state
                .workspace_alias
                .or_else(|| self.default_workspace_alias.clone())),
            Err(error) => {
                tracing::error!(channel = %key.channel, peer = %key.peer, error = %error, "Telegram chat state could not be read");
                Err("Der Chat-Zustand konnte nicht gelesen werden.".to_owned())
            }
        }
    }

    /// Legt einen Arbeitsauftrag an und sendet die Freigabe-Schaltflächen
    /// (`request_id = "work:" + WorkId`, Genehmiger = Anfragender). Scheitert
    /// die Schaltflächen-Zustellung, folgt eine Textantwort mit den
    /// Befehlen.
    #[allow(clippy::too_many_arguments)]
    fn submit_work(
        &self,
        key: &SessionKey,
        event: &InboundEvent,
        actor: &WorkRequestActor,
        target: &ReplyTarget,
        alias: &str,
        role: &str,
        task: &str,
        now: Timestamp,
    ) {
        let submitted = self.work_requests.submit(
            &key.channel,
            &actor.peer,
            &key.tenant,
            alias,
            role,
            task,
            event.raw_event_id.as_deref().unwrap_or_default(),
            self.workspaces.as_ref(),
            now,
        );
        let record = match submitted {
            Ok(record) => record,
            Err(error) => {
                tracing::warn!(channel = %key.channel, peer = %key.peer, error = %error, "Telegram work request submission failed");
                self.reply(key, target, work_request_error_text(&error));
                return;
            }
        };
        tracing::info!(channel = %key.channel, peer = %key.peer, work_id = %record.work_id, "Telegram work request submitted");
        let prompt = work_approval_prompt(&record, alias);
        match self
            .outbound
            .send_approval(target.chat_id, target.thread_id, &prompt, &actor.peer)
        {
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(channel = %key.channel, peer = %key.peer, error = %error, "Telegram work request approval buttons could not be delivered");
                self.reply(
                    key,
                    target,
                    format!(
                        "Arbeitsauftrag {id} angefragt (Arbeitsbereich {alias}, Rolle {role}). \
                         Prüfen mit /review {id}, freigeben mit /approve {id}, ablehnen mit /deny {id}.",
                        id = record.work_id,
                        role = record.role,
                    ),
                );
            }
        }
    }

    fn lifecycle(
        &self,
        action: &'static str,
        work_id: &str,
        result: Result<String, TelegramChannelError>,
    ) -> String {
        match result {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(action, work_id, error = %error, "Telegram work-request command failed");
                work_request_error_text(&error)
            }
        }
    }

    fn workspace(&self, key: &SessionKey, alias: Option<&str>) -> String {
        match alias {
            None => match self.effective_workspace(key) {
                Ok(Some(alias)) => {
                    format!("Aktueller Arbeitsbereich: {alias}. Wechseln mit /workspace <alias>.")
                }
                Ok(None) => {
                    "Kein Arbeitsbereich gewählt. Wählen mit /workspace <alias>.".to_owned()
                }
                Err(reply) => reply,
            },
            Some(alias) => {
                match self
                    .chat_state
                    .set_workspace(key, Some(alias), self.workspaces.as_ref())
                {
                    Ok(_) => format!("Arbeitsbereich gewechselt: {alias}"),
                    Err(error) => {
                        tracing::warn!(channel = %key.channel, peer = %key.peer, error = %error, "Telegram workspace selection failed");
                        work_request_error_text(&error)
                    }
                }
            }
        }
    }

    fn status(&self, key: &SessionKey, actor: &WorkRequestActor) -> String {
        let mut lines = Vec::new();
        match self.chat_state.get(key) {
            Ok(state) => {
                let workspace = state
                    .workspace_alias
                    .or_else(|| {
                        self.default_workspace_alias
                            .as_ref()
                            .map(|alias| format!("{alias} (Standard)"))
                    })
                    .unwrap_or_else(|| "keiner".to_owned());
                lines.push(format!("Arbeitsbereich: {workspace}"));
                lines.push(format!(
                    "Unterhaltung: Nr. {}",
                    state.session_generation.saturating_add(1)
                ));
            }
            Err(error) => {
                tracing::error!(channel = %key.channel, peer = %key.peer, error = %error, "Telegram chat state could not be read");
                lines.push("Chat-Zustand nicht lesbar.".to_owned());
            }
        }
        match self.work_requests.list_for(actor, STATUS_WORK_LIMIT) {
            Ok(records) if records.is_empty() => {
                lines.push("Keine Arbeitsaufträge.".to_owned());
            }
            Ok(records) => {
                lines.push("Letzte Arbeitsaufträge:".to_owned());
                for record in records {
                    lines.push(format!(
                        "- {} · {} · {} in {}",
                        record.work_id,
                        state_label(record.state),
                        record.role,
                        record.workspace
                    ));
                }
            }
            Err(error) => {
                tracing::error!(channel = %key.channel, peer = %key.peer, error = %error, "Telegram work requests could not be listed");
                lines.push("Arbeitsaufträge nicht lesbar.".to_owned());
            }
        }
        lines.join("\n")
    }

    fn reply(&self, key: &SessionKey, target: &ReplyTarget, markdown: String) {
        if let Err(error) = self.outbound.send(
            target.chat_id,
            target.thread_id,
            &OutboundContent::Message { markdown },
        ) {
            tracing::error!(channel = %key.channel, peer = %key.peer, error = %error, "Telegram command reply delivery failed");
        }
    }
}

fn reply_target(event: &InboundEvent) -> Option<ReplyTarget> {
    let chat_id = event.peer.as_str().parse::<i64>().ok()?;
    let thread_id = event
        .thread
        .as_ref()
        .and_then(|thread| thread.as_str().parse::<i64>().ok());
    Some(ReplyTarget { chat_id, thread_id })
}

/// Kleingeschriebener Befehlsname ohne `/` und `@bot`-Suffix.
fn command_name(text: &str) -> Option<String> {
    let token = text.split_whitespace().next()?;
    let name = token.strip_prefix('/')?;
    let name = name.split_once('@').map_or(name, |(name, _)| name);
    Some(name.to_ascii_lowercase())
}

/// Die Freigabe-Aufforderung eines frisch angelegten Arbeitsauftrags. Der
/// Aufgabentext selbst wird nicht wiederholt (der Datensatz kennt nur den
/// Digest).
fn work_approval_prompt(record: &WorkRequestRecord, alias: &str) -> ApprovalPrompt {
    ApprovalPrompt {
        request_id: format!("{WORK_APPROVAL_PREFIX}{}", record.work_id),
        summary: format!(
            "Arbeitsauftrag {id}\nArbeitsbereich: {alias}\nRolle: {role}\n\
             Alternativ: /approve {id} bzw. /deny {id}",
            id = record.work_id,
            role = record.role,
        ),
        risk: "Sandbox-Ausführung".to_owned(),
        actions: vec![
            ApprovalAction {
                label: "Freigeben".to_owned(),
                decision: "approve".to_owned(),
            },
            ApprovalAction {
                label: "Ablehnen".to_owned(),
                decision: "deny".to_owned(),
            },
        ],
    }
}

/// Deutsche Kurzbezeichnung eines Auftragszustands.
fn state_label(state: harw_channel_telegram::WorkRequestState) -> &'static str {
    use harw_channel_telegram::WorkRequestState as State;
    match state {
        State::Requested => "angefragt",
        State::UnderReview => "in Prüfung",
        State::Approved => "freigegeben",
        State::Launched => "gestartet",
        State::Denied => "abgelehnt",
        State::Cancelled => "abgebrochen",
    }
}

/// Nutzertext zu einem Fehler des Work-Request- bzw. Chat-State-Stores.
/// Enthält nie Pfade oder interne Fehlertexte.
pub(super) fn work_request_error_text(error: &TelegramChannelError) -> String {
    match error {
        TelegramChannelError::WorkRequestNotFound { work_id } => {
            format!("Arbeitsauftrag {work_id} nicht gefunden.")
        }
        TelegramChannelError::WorkRequestInvalidTransition {
            work_id,
            from,
            action,
        } => format!(
            "Aktion „{action}“ ist für Arbeitsauftrag {work_id} im Zustand „{from}“ nicht möglich."
        ),
        TelegramChannelError::WorkspaceUnresolved { alias, .. } => {
            format!("Arbeitsbereich „{alias}“ ist für diesen Chat nicht verfügbar.")
        }
        TelegramChannelError::InvalidWorkRequestRole { role } => {
            format!("Ungültige Rolle „{role}“.")
        }
        TelegramChannelError::LaunchNotYetAvailable { work_id } => format!(
            "Arbeitsauftrag {work_id} ist freigegeben, kann aber derzeit nicht gestartet werden."
        ),
        _ => "Anfrage fehlgeschlagen (interner Fehler).".to_owned(),
    }
}

/// Die Befehlsübersicht für `/help`, `/start` und unbekannte Befehle.
pub(super) fn help_text() -> String {
    let mut text = String::from("Verfügbare Befehle:\n");
    for spec in HANDLED_COMMANDS {
        text.push_str(spec.usage);
        text.push_str(" – ");
        text.push_str(
            spec.description
                .split(':')
                .next()
                .unwrap_or(spec.description),
        );
        text.push('\n');
    }
    text.push_str("\nAndere Nachrichten gehen direkt an den Assistenten.");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{WorkspaceRegistration, WorkspaceRegistry};
    use harw_channel::{SenderRef, ThreadRef};
    use harw_channel_telegram_transport::TransportResult;
    use harw_types::{ChannelId, TenantId, WorkspaceId};
    use std::path::PathBuf;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingOutbound {
        messages: Mutex<Vec<(i64, Option<i64>, String)>>,
        approvals: Mutex<Vec<(i64, ApprovalPrompt, PeerId)>>,
        fail_approval: bool,
    }

    impl RecordingOutbound {
        fn messages(&self) -> Vec<(i64, Option<i64>, String)> {
            self.messages
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default()
        }

        fn approvals(&self) -> Vec<(i64, ApprovalPrompt, PeerId)> {
            self.approvals
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default()
        }
    }

    impl TelegramOutbound for RecordingOutbound {
        fn send(
            &self,
            chat_id: i64,
            thread_id: Option<i64>,
            content: &OutboundContent,
        ) -> TransportResult<()> {
            if let (Ok(mut messages), OutboundContent::Message { markdown }) =
                (self.messages.lock(), content)
            {
                messages.push((chat_id, thread_id, markdown.clone()));
            }
            Ok(())
        }

        fn edit(&self, _: i64, _: i64, _: &OutboundContent) -> TransportResult<()> {
            Ok(())
        }

        fn send_approval(
            &self,
            chat_id: i64,
            _thread_id: Option<i64>,
            prompt: &ApprovalPrompt,
            approver: &PeerId,
        ) -> TransportResult<i64> {
            if self.fail_approval {
                return Err(
                    harw_channel_telegram_transport::TelegramTransportError::ApiRejected {
                        method: "send_approval",
                        code: 0,
                        description: "unsupported by this outbound".into(),
                    },
                );
            }
            if let Ok(mut approvals) = self.approvals.lock() {
                approvals.push((chat_id, prompt.clone(), approver.clone()));
            }
            Ok(77)
        }
    }

    struct Fixture {
        _harness: tempfile::TempDir,
        _state: tempfile::TempDir,
        outbound: Arc<RecordingOutbound>,
        handler: TelegramCommandHandler,
        key: SessionKey,
    }

    fn fixture(
        fallback: UnknownCommandFallback,
        default_alias: Option<&str>,
        fail_approval: bool,
    ) -> TestResult<Fixture> {
        let harness = tempfile::tempdir().map_err(ctx("harness dir"))?;
        let state = tempfile::tempdir().map_err(ctx("state dir"))?;
        std::fs::create_dir(harness.path().join("ops")).map_err(ctx("workspace dir"))?;
        let tenant = TenantId::from_str("ops");
        let registry = WorkspaceRegistry::build(
            harness.path(),
            [WorkspaceRegistration {
                tenant: tenant.clone(),
                workspace: WorkspaceId::from_str("ops-room"),
                root: PathBuf::from("ops"),
            }],
        )
        .map_err(ctx("registry"))?;
        let outbound = Arc::new(RecordingOutbound {
            fail_approval,
            ..RecordingOutbound::default()
        });
        let handler = TelegramCommandHandler {
            outbound: Arc::clone(&outbound) as Arc<dyn TelegramOutbound>,
            work_requests: Arc::new(WorkRequestStore::new(&state.path().join("work"))),
            workspaces: Arc::new(registry),
            chat_state: Arc::new(ChatStateStore::new(&state.path().join("chat"))),
            admin_sender_ids: HashSet::from(["900".to_owned()]),
            unknown_command_fallback: fallback,
            default_workspace_alias: default_alias.map(str::to_owned),
        };
        let key = SessionKey::new(
            tenant,
            ChannelId::from_str("telegram:ops"),
            PeerId::from_str("100"),
            None,
        );
        Ok(Fixture {
            _harness: harness,
            _state: state,
            outbound,
            handler,
            key,
        })
    }

    fn event(sender: &str, text: &str) -> InboundEvent {
        InboundEvent {
            channel: ChannelId::from_str("telegram:ops"),
            peer: PeerId::from_str("100"),
            thread: None,
            sender: Some(SenderRef {
                id: sender.to_owned(),
                display_name: None,
            }),
            text: Some(text.to_owned()),
            mentioned: false,
            attachments: Vec::new(),
            raw_event_id: Some("1".to_owned()),
            received_at: Timestamp::now(),
        }
    }

    fn run(fixture: &Fixture, sender: &str, text: &str) -> CommandDisposition {
        fixture
            .handler
            .handle(&fixture.key, &event(sender, text), text)
    }

    fn last_message(fixture: &Fixture) -> TestResult<String> {
        fixture
            .outbound
            .messages()
            .last()
            .map(|(_, _, text)| text.clone())
            .ok_or(TestError::Missing("a reply"))
    }

    fn toml_binding(extra: &str) -> TestResult<TelegramChannelToml> {
        let source =
            format!("id = \"telegram:ops\"\nbot_token_ref = \"env:HARW_TEST_TOKEN\"\n{extra}");
        toml::from_str(&source).map_err(ctx("binding parses"))
    }

    #[test]
    fn policy_visible_plan_scopes_menus_by_visibility() -> TestResult {
        let binding = toml_binding(
            "[security]\npinned_identities = [42, 7, 42]\nadmin_identities = [7]\n\
             [groups]\nallowed_chats = [\"-1001\"]\n",
        )?;
        let plan = telegram_menu_plan(&binding).map_err(TestError::Unexpected)?;
        let names = |scope: &BotCommandScope| -> Option<Vec<String>> {
            plan.iter()
                .find(|(candidate, _)| candidate == scope)
                .map(|(_, commands)| commands.iter().map(|c| c.command.clone()).collect())
        };
        assert_eq!(
            names(&BotCommandScope::Default),
            Some(vec!["help".to_owned(), "start".to_owned()])
        );
        assert_eq!(
            names(&BotCommandScope::AllGroupChats),
            Some(vec!["help".to_owned(), "start".to_owned()])
        );
        assert_eq!(
            names(&BotCommandScope::AllPrivateChats),
            Some(vec![
                "help".to_owned(),
                "start".to_owned(),
                "pair".to_owned()
            ])
        );
        let full: Vec<String> = telegram_handled_commands()
            .into_iter()
            .map(|command| command.command)
            .collect();
        assert_eq!(
            names(&BotCommandScope::Chat { chat_id: 42 }),
            Some(full.clone())
        );
        assert_eq!(
            names(&BotCommandScope::Chat { chat_id: 7 }),
            Some(full.clone())
        );
        assert_eq!(
            names(&BotCommandScope::Chat { chat_id: -1001 }),
            Some(full.clone())
        );
        assert_eq!(
            names(&BotCommandScope::ChatAdministrators { chat_id: -1001 }),
            Some(full)
        );
        // Doppelte Identitäten erzeugen keinen doppelten Scope.
        assert_eq!(plan.len(), 3 + 2 + 2);
        Ok(())
    }

    #[test]
    fn policy_visible_plan_skips_admin_scope_without_admins_and_rejects_bad_groups() -> TestResult {
        let binding = toml_binding("[groups]\nallowed_chats = [\"-5\"]\n")?;
        let plan = telegram_menu_plan(&binding).map_err(TestError::Unexpected)?;
        assert!(
            !plan
                .iter()
                .any(|(scope, _)| matches!(scope, BotCommandScope::ChatAdministrators { .. }))
        );
        let bad = toml_binding("[groups]\nallowed_chats = [\"not-a-chat\"]\n")?;
        assert!(telegram_menu_plan(&bad).is_err());
        Ok(())
    }

    #[test]
    fn static_none_and_unknown_menu_sources() -> TestResult {
        let static_plan =
            telegram_menu_plan(&toml_binding("[commands]\nmenu_source = \"static\"\n")?)
                .map_err(TestError::Unexpected)?;
        assert_eq!(
            static_plan,
            vec![(BotCommandScope::Default, telegram_handled_commands())]
        );
        let none = telegram_menu_plan(&toml_binding("[commands]\nmenu_source = \"none\"\n")?)
            .map_err(TestError::Unexpected)?;
        assert!(none.is_empty());
        let mut bogus = toml_binding("")?;
        bogus.commands.menu_source = "bogus".to_owned();
        assert!(telegram_menu_plan(&bogus).is_err());
        Ok(())
    }

    #[test]
    fn menu_lists_only_commands_the_parser_accepts() {
        for command in telegram_handled_commands() {
            assert!((1..=32).contains(&command.command.len()));
            assert!(
                command
                    .command
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            );
            assert!((3..=256).contains(&command.description.chars().count()));
            let sample = match command.command.as_str() {
                "request" => "/request ws role task".to_owned(),
                "task" => "/task role task".to_owned(),
                "review" | "approve" | "deny" | "cancel" => format!("/{} w-1", command.command),
                other => format!("/{other}"),
            };
            assert!(
                parse_command(&sample).is_some(),
                "menu advertises /{} but the parser rejects it",
                command.command
            );
        }
        assert!(
            !telegram_handled_commands()
                .iter()
                .any(|command| command.command == "pair")
        );
    }

    #[test]
    fn unknown_command_fallback_parses_config_values() {
        assert_eq!(
            UnknownCommandFallback::parse("reply_help"),
            Ok(UnknownCommandFallback::ReplyHelp)
        );
        assert_eq!(
            UnknownCommandFallback::parse("pass_through"),
            Ok(UnknownCommandFallback::PassThrough)
        );
        assert_eq!(
            UnknownCommandFallback::parse("ignore"),
            Ok(UnknownCommandFallback::Ignore)
        );
        assert!(UnknownCommandFallback::parse("bogus").is_err());
    }

    #[test]
    fn plain_text_is_not_a_command() -> TestResult {
        let fixture = fixture(UnknownCommandFallback::ReplyHelp, None, false)?;
        assert_eq!(
            run(&fixture, "100", "hallo /etc/passwd"),
            CommandDisposition::NotACommand
        );
        assert_eq!(run(&fixture, "100", "/"), CommandDisposition::NotACommand);
        assert!(fixture.outbound.messages().is_empty());
        Ok(())
    }

    #[test]
    fn unknown_commands_follow_the_configured_fallback() -> TestResult {
        let help = fixture(UnknownCommandFallback::ReplyHelp, None, false)?;
        assert_eq!(
            run(&help, "100", "/frobnicate"),
            CommandDisposition::Handled
        );
        assert!(last_message(&help)?.contains("/help"));

        let pass = fixture(UnknownCommandFallback::PassThrough, None, false)?;
        assert_eq!(
            run(&pass, "100", "/frobnicate"),
            CommandDisposition::NotACommand
        );
        assert!(pass.outbound.messages().is_empty());

        let ignore = fixture(UnknownCommandFallback::Ignore, None, false)?;
        assert_eq!(
            run(&ignore, "100", "/frobnicate"),
            CommandDisposition::Handled
        );
        assert!(ignore.outbound.messages().is_empty());

        // Bekannter Befehl mit ungültigen Argumenten: immer Syntaxhinweis.
        assert_eq!(
            run(&pass, "100", "/request nur"),
            CommandDisposition::Handled
        );
        assert!(last_message(&pass)?.contains("/request <workspace>"));
        // `/pair` wird nie beantwortet.
        let before = help.outbound.messages().len();
        assert_eq!(run(&help, "100", "/pair ABCD"), CommandDisposition::Handled);
        assert_eq!(run(&help, "100", "/pair"), CommandDisposition::Handled);
        assert_eq!(help.outbound.messages().len(), before);
        Ok(())
    }

    #[test]
    fn task_uses_default_workspace_and_sends_bound_approval() -> TestResult {
        let fixture = fixture(UnknownCommandFallback::ReplyHelp, Some("ops-room"), false)?;
        assert_eq!(
            run(&fixture, "100", "/task implementer fix the build"),
            CommandDisposition::Handled
        );
        let approvals = fixture.outbound.approvals();
        let (chat_id, prompt, approver) = approvals
            .first()
            .ok_or(TestError::Missing("approval prompt"))?;
        assert_eq!(*chat_id, 100);
        assert_eq!(approver, &PeerId::from_str("100"));
        let work_id = prompt
            .request_id
            .strip_prefix("work:")
            .ok_or(TestError::Missing("work: prefix"))?;
        assert!(!prompt.summary.contains("fix the build"));
        let decisions: Vec<&str> = prompt
            .actions
            .iter()
            .map(|action| action.decision.as_str())
            .collect();
        assert_eq!(decisions, vec!["approve", "deny"]);
        assert!(fixture.outbound.messages().is_empty());

        // Ein fremder Nicht-Admin sieht den Auftrag nicht.
        run(&fixture, "555", &format!("/deny {work_id}"));
        assert!(last_message(&fixture)?.contains("nicht gefunden"));
        // Der Anfragende darf ablehnen.
        run(&fixture, "100", &format!("/deny {work_id}"));
        assert!(!last_message(&fixture)?.contains("nicht gefunden"));
        Ok(())
    }

    #[test]
    fn task_without_workspace_asks_for_selection_and_workspace_switch_is_checked() -> TestResult {
        let fixture = fixture(UnknownCommandFallback::ReplyHelp, None, false)?;
        run(&fixture, "100", "/task implementer fix it");
        assert!(last_message(&fixture)?.contains("Kein Arbeitsbereich"));
        assert!(fixture.outbound.approvals().is_empty());

        run(&fixture, "100", "/workspace other");
        assert!(last_message(&fixture)?.contains("nicht verfügbar"));
        run(&fixture, "100", "/workspace ops-room");
        assert!(last_message(&fixture)?.contains("gewechselt"));
        run(&fixture, "100", "/workspace");
        assert!(last_message(&fixture)?.contains("ops-room"));
        run(&fixture, "100", "/task implementer fix it");
        assert_eq!(fixture.outbound.approvals().len(), 1);
        Ok(())
    }

    #[test]
    fn failed_approval_delivery_falls_back_to_text() -> TestResult {
        let fixture = fixture(UnknownCommandFallback::ReplyHelp, None, true)?;
        run(&fixture, "100", "/request ops-room implementer fix it");
        let reply = last_message(&fixture)?;
        assert!(reply.contains("/approve"));
        assert!(!reply.contains("fix it"));
        Ok(())
    }

    #[test]
    fn new_status_help_and_start_reply() -> TestResult {
        let fixture = fixture(UnknownCommandFallback::ReplyHelp, Some("ops-room"), false)?;
        run(&fixture, "100", "/new");
        assert!(last_message(&fixture)?.contains("Neue Unterhaltung"));
        let generation = fixture
            .handler
            .chat_state
            .get(&fixture.key)
            .map_err(ctx("chat state"))?
            .session_generation;
        assert_eq!(generation, 1);
        run(&fixture, "100", "/status");
        let status = last_message(&fixture)?;
        assert!(status.contains("ops-room (Standard)"));
        assert!(status.contains("Keine Arbeitsaufträge"));
        run(&fixture, "100", "/help");
        assert!(last_message(&fixture)?.contains("/workspace [alias]"));
        run(&fixture, "100", "/start@HarwBot deeplink");
        assert!(last_message(&fixture)?.starts_with("Hallo"));
        Ok(())
    }

    #[test]
    fn commands_reply_into_the_event_thread() -> TestResult {
        let fixture = fixture(UnknownCommandFallback::ReplyHelp, None, false)?;
        let mut threaded = event("100", "/help");
        threaded.thread = Some(ThreadRef::from_str("9"));
        fixture.handler.handle(&fixture.key, &threaded, "/help");
        let (chat_id, thread_id, _) = fixture
            .outbound
            .messages()
            .last()
            .cloned()
            .ok_or(TestError::Missing("reply"))?;
        assert_eq!((chat_id, thread_id), (100, Some(9)));
        Ok(())
    }

    #[test]
    fn commands_without_sender_fail_closed() -> TestResult {
        let fixture = fixture(UnknownCommandFallback::ReplyHelp, Some("ops-room"), false)?;
        let mut anonymous = event("100", "/task implementer fix");
        anonymous.sender = None;
        assert_eq!(
            fixture
                .handler
                .handle(&fixture.key, &anonymous, "/task implementer fix"),
            CommandDisposition::Handled
        );
        assert!(fixture.outbound.approvals().is_empty());
        assert!(fixture.outbound.messages().is_empty());
        Ok(())
    }
}

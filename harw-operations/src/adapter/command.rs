//! `CommandAdapter` — exponiert eine [`Operation`] als `/`-Slash-Command.
//!
//! # Warum eine Op mehrere `CommandAdapter` erzeugen kann
//!
//! Eine [`Operation`] darf in ihrer [`crate::operation::OperationMeta::surfaces`]-Liste
//! **mehrere** [`crate::operation::Surface::Command`]-Einträge deklarieren —
//! etwa einen mit [`crate::operation::CommandVisibility::TuiOnly`] unter Pfad `"/status"`
//! und einen zweiten mit [`crate::operation::CommandVisibility::ChannelReduced`] unter
//! Pfad `"/status-brief"`. [`CommandAdapter::from_operation`] erzeugt pro
//! `Surface::Command`-Eintrag genau **einen** Adapter, der Pfad und Sichtbarkeit aus
//! dem jeweiligen Surface-Eintrag bezieht. TUI-Dispatcher und Channel-Dispatcher
//! registrieren damit unabhängig voneinander, ohne die Op selbst zu kennen.
//!
//! # Verantwortungsbereich
//! - Filtert `Surface::Command`-Einträge aus `OperationMeta::surfaces`.
//! - Baut `OpInput { raw_args, json_args: Value::Null }` und delegiert an
//!   `Operation::run`.
//! - Kein Rendering, kein Output-Formatting — das bleibt höheren Crates
//!   (`harw-tui`, `harw-core`) vorbehalten.
//!
//! # Schlüsseltypen
//! - [`CommandAdapter`] — der Adapter selbst
//!
//! # Nebenläufigkeit
//! [`CommandAdapter`] ist `Send + Sync`: enthält `Arc<dyn Operation>` (selbst
//! `Send + Sync`), `&'static str` und `Copy`-Enums ohne inneren Zustand.
//!
//! # Fehlertypen
//! [`crate::error::OpError`] — wird 1:1 von `Operation::run` weitergeleitet.
//!
//! # Beispiel
//! ```rust,no_run
//! use std::sync::Arc;
//! use harw_operations::adapter::CommandAdapter;
//! use harw_operations::operation::{
//!     BusyAvailability, CommandVisibility, OpFuture, OpInput, OpOutput, Operation,
//!     OperationCategory, OperationDomain, OperationMeta, PermissionTier, Surface,
//! };
//! use harw_operations::context::OpContext;
//!
//! struct MyOp;
//! impl Operation for MyOp {
//!     fn meta(&self) -> &OperationMeta {
//!         static M: std::sync::OnceLock<OperationMeta> = std::sync::OnceLock::new();
//!         M.get_or_init(|| OperationMeta {
//!             name: "my.op",
//!             summary: "Beispiel-Operation.",
//!             domain: OperationDomain::Misc,
//!             permission: PermissionTier::Observer,
//!             surfaces: vec![Surface::Command {
//!                 path: "/my",
//!                 visibility: CommandVisibility::TuiOnly,
//!             }],
//!             aliases: &[],
//!             category: OperationCategory::Misc,
//!             args_schema: None,
//!             output_schema: None,
//!             busy: BusyAvailability::DeferredUntilTurnEnd,
//!         })
//!     }
//!     fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
//!         Box::pin(async { Ok(OpOutput::from("ok".to_owned())) })
//!     }
//! }
//!
//! let adapters = CommandAdapter::from_operation(Arc::new(MyOp));
//! assert_eq!(adapters.len(), 1);
//! assert_eq!(adapters[0].path(), "/my");
//! ```

use std::sync::Arc;

use crate::context::OpContext;
use crate::error::OpError;
use crate::operation::{
    CommandVisibility, OpInput, OpOutput, Operation, OperationMeta, PermissionTier, RemoteGrant,
    RemotePolicy, RemoteRefusal, RemoteSlice, RemoteVerb, Surface,
};

/// Adapter, der eine [`Operation`] als `/`-Slash-Command exponiert.
///
/// # Beschreibung
/// Ein `CommandAdapter` repräsentiert **eine** `Surface::Command`-Deklaration
/// aus [`OperationMeta::surfaces`]. Mehrere Adapter für dieselbe Op sind möglich,
/// wenn die Op mehrere `Surface::Command`-Einträge deklariert (z. B. TUI und Channel).
///
/// Der Adapter führt kein Rendering durch — er baut ausschließlich `OpInput` und
/// delegiert den Aufruf an [`Operation::run`].
///
/// # Nebenläufigkeit
/// `CommandAdapter` ist `Send + Sync`: enthält `Arc<dyn Operation>` (selbst
/// `Send + Sync`), `&'static str` und `Copy`-Enums ohne inneren Zustand.
///
/// # Fehlertypen
/// [`OpError`] — wird in [`Self::dispatch`] 1:1 von `Operation::run` weitergeleitet.
///
/// # Beispiel
/// ```rust,no_run
/// // Erzeugung über from_operation; direkter Konstruktor ist nicht öffentlich.
/// use std::sync::Arc;
/// use harw_operations::adapter::CommandAdapter;
/// ```
pub struct CommandAdapter {
    op: Arc<dyn Operation>,
    path: &'static str,
    visibility: CommandVisibility,
    permission: PermissionTier,
    operation_name: &'static str,
}

impl CommandAdapter {
    /// Erzeugt einen `CommandAdapter` pro `Surface::Command`-Eintrag der übergebenen Operation.
    ///
    /// # Beschreibung
    /// Iteriert über [`OperationMeta::surfaces`] und erzeugt für jeden
    /// `Surface::Command { path, visibility }`-Eintrag einen Adapter.
    /// Die Berechtigung wird einmalig aus [`OperationMeta::permission`] übernommen
    /// und gilt für alle erzeugten Adapter dieser Operation.
    /// `Surface::ModelTool`-Einträge werden stillschweigend ignoriert.
    ///
    /// # Argumente
    /// - `op` (`Arc<dyn Operation>`): Die Operation, die als Command exponiert werden soll.
    ///   Der `Arc` wird pro gefundener `Surface::Command` mit `Arc::clone` dupliziert —
    ///   die Operation selbst wird nicht geklont.
    ///
    /// # Returns
    /// Ein `Vec<CommandAdapter>` mit einem Eintrag pro `Surface::Command`-Deklaration.
    /// Operationen ohne `Surface::Command`-Eintrag ergeben einen leeren `Vec`.
    ///
    /// # Errors
    /// Keine — diese Funktion schlägt nicht fehl.
    ///
    /// # Concurrency
    /// Sicher in Multi-Thread-Umgebungen: nur lesender Zugriff auf `meta()` und
    /// atomares Inkrementieren des `Arc`-Referenzzählers per `Arc::clone`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use harw_operations::adapter::CommandAdapter;
    /// // Ops ohne Surface::Command liefern einen leeren Vec:
    /// // let adapters = CommandAdapter::from_operation(Arc::new(NoSurfaceOp));
    /// // assert!(adapters.is_empty());
    /// ```
    #[must_use]
    pub fn from_operation(op: Arc<dyn Operation>) -> Vec<Self> {
        let meta: &OperationMeta = op.meta();
        let permission = meta.permission;
        let operation_name = meta.name;
        meta.surfaces
            .iter()
            .filter_map(|surface| {
                if let Surface::Command { path, visibility } = surface {
                    Some(Self {
                        op: Arc::clone(&op),
                        path,
                        visibility: *visibility,
                        permission,
                        operation_name,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Gibt den eingetragenen Command-Pfad zurück.
    ///
    /// # Beschreibung
    /// Liefert den statischen Pfad-String aus `Surface::Command { path, .. }`,
    /// z. B. `"/status"` oder `"/session/list"`.
    ///
    /// # Returns
    /// `&str` — der Pfad-String. Lebensdauer ist an `&self` gebunden (`'static` intern).
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // assert_eq!(adapter.path(), "/session/list");
    /// ```
    #[must_use]
    pub fn path(&self) -> &str {
        self.path
    }

    /// Gibt die Sichtbarkeit dieses Commands zurück.
    ///
    /// # Beschreibung
    /// Liefert den [`CommandVisibility`]-Wert aus der `Surface::Command`-Deklaration,
    /// der steuert, ob der Command in der TUI, in Channels oder in beidem erscheint.
    ///
    /// # Returns
    /// [`CommandVisibility`] — `Copy`, keine Allokation.
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // assert_eq!(adapter.visibility(), CommandVisibility::TuiOnly);
    /// ```
    #[must_use]
    pub fn visibility(&self) -> CommandVisibility {
        self.visibility
    }

    /// Gibt die aus `OperationMeta::permission` übernommene Mindest-Berechtigungsstufe zurück.
    ///
    /// # Beschreibung
    /// Bestimmt, welche Caller-Stufe zur Ausführung des Commands mindestens nötig ist.
    /// Der Wert wird einmalig beim Erzeugen des Adapters aus `OperationMeta::permission`
    /// kopiert und ändert sich nicht.
    ///
    /// # Returns
    /// [`PermissionTier`] — `Copy`, keine Allokation.
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // assert_eq!(adapter.permission(), PermissionTier::Observer);
    /// ```
    #[must_use]
    pub fn permission(&self) -> PermissionTier {
        self.permission
    }

    /// Prüft, ob dieser konkrete Aufruf auf einer Channel-Fläche zulässig ist.
    ///
    /// `TuiOnly` ist immer gesperrt, `ChannelParity` übernimmt die gesamte
    /// Command-Grammatik. `ChannelReduced` ist fail-closed und erlaubt nur
    /// die von [`Operation::channel_subcommands`] deklarierten Formen
    /// (geprüft wird das erste Argument-Token).
    ///
    /// Zusätzlich wird die Sperrliste [`Operation::channel_denied_tokens`]
    /// über die **gesamte** Argumentliste geprüft: erscheint eines der
    /// gesperrten Tokens an irgendeiner Position, ist der Aufruf verboten.
    /// Das Wildcard `"*"` in `channel_subcommands` umgeht diese Sperrliste
    /// nicht.
    #[must_use]
    pub fn allows_channel_invocation(&self, raw_args: &[String]) -> bool {
        match self.visibility {
            CommandVisibility::TuiOnly => false,
            CommandVisibility::ChannelParity => true,
            CommandVisibility::ChannelReduced => {
                let denied = self.op.channel_denied_tokens();
                if raw_args.iter().any(|t| denied.contains(&t.as_str())) {
                    return false;
                }
                let allowed = self.op.channel_subcommands();
                if allowed.contains(&"*") {
                    return true;
                }
                let first = raw_args.first().map(String::as_str).unwrap_or("-");
                allowed.contains(&first)
            }
        }
    }

    /// Prüft einen Remote-Aufruf (Handy) gegen die Remote-Allowlist des Befehls.
    ///
    /// # Beschreibung
    /// Die Remote-Fläche ist eine eigene Achse neben [`CommandVisibility`]: auch ein
    /// `TuiOnly`-Befehl ist mit passender [`RemotePolicy`] remote-fähig, und ein
    /// `ChannelParity`-Befehl ohne Policy ist es nicht (fail-closed).
    /// [`Self::allows_channel_invocation`] bleibt davon unberührt.
    ///
    /// Reihenfolge der Prüfungen (der erste Fehlschlag gewinnt):
    /// 1. `policy` ist `None` oder `policy.path != self.path()` → [`RemoteRefusal::NotRemote`]
    /// 2. ein Token aus `policy.denied_tokens` an beliebiger Position → [`RemoteRefusal::DeniedToken`]
    /// 3. `policy.find_verb(raw_args)` ist `None` → [`RemoteRefusal::VerbNotRemote`]
    /// 4. `verb.slice > enabled` → [`RemoteRefusal::SliceDisabled`]
    /// 5. `!grant.has(verb.cap)` → [`RemoteRefusal::MissingCap`]
    /// 6. `grant.tier < verb.tier` → [`RemoteRefusal::TierTooLow`]
    /// 7. sonst `Ok(verb)`
    ///
    /// Step-up (`verb.stepup`) prüft der Aufrufer, nicht der Adapter.
    ///
    /// # Arguments
    /// - `policy`: Remote-Allowlist des Befehls, `None` = nicht remote.
    /// - `grant`: wirksame Rechte des Remote-Aufrufers.
    /// - `enabled`: höchste freigeschaltete Lieferscheibe.
    /// - `raw_args`: Argument-Tokens ohne Befehlsnamen.
    ///
    /// # Errors
    /// [`RemoteRefusal`] nach der obigen Reihenfolge. Bei Ablehnung wird
    /// `operation.command.remote_refused` auf `debug` geloggt — ohne Argumente,
    /// da diese Werte enthalten können.
    ///
    /// # Concurrency
    /// Rein lesend, lock-frei, keine Allokation im Erfolgsfall.
    pub fn allows_remote_invocation(
        &self,
        policy: Option<&RemotePolicy>,
        grant: &RemoteGrant,
        enabled: RemoteSlice,
        raw_args: &[String],
    ) -> Result<&'static RemoteVerb, RemoteRefusal> {
        let result = self.check_remote_invocation(policy, grant, enabled, raw_args);
        if let Err(refusal) = &result {
            tracing::debug!(
                op = self.operation_name,
                path = self.path,
                code = refusal.code(),
                "operation.command.remote_refused"
            );
        }
        result
    }

    /// Ob der Befehl für diesen Aufrufer überhaupt eine erlaubte Remote-Form hat
    /// (für `commands.list.available`).
    ///
    /// # Returns
    /// - `Ok(())`, wenn mindestens ein Verb die Prüfungen 4-6 aus
    ///   [`Self::allows_remote_invocation`] besteht.
    /// - sonst die Ablehnung des **ersten** Verbs in Deklarationsreihenfolge
    ///   (deterministisch).
    /// - ohne Policy, mit fremdem Pfad oder ohne ein einziges Verb: [`RemoteRefusal::NotRemote`].
    ///
    /// Gesperrte Tokens spielen hier keine Rolle (es gibt keine Argumente).
    /// Kein Logging: die Listen-Projektion fragt jeden Befehl ab.
    ///
    /// # Errors
    /// Siehe Returns.
    pub fn remote_availability(
        &self,
        policy: Option<&RemotePolicy>,
        grant: &RemoteGrant,
        enabled: RemoteSlice,
    ) -> Result<(), RemoteRefusal> {
        let policy = self.matching_policy(policy)?;
        let mut first_refusal: Option<RemoteRefusal> = None;
        for verb in policy.verbs {
            match check_remote_verb(verb, grant, enabled) {
                Ok(()) => return Ok(()),
                Err(refusal) => {
                    if first_refusal.is_none() {
                        first_refusal = Some(refusal);
                    }
                }
            }
        }
        Err(first_refusal.unwrap_or(RemoteRefusal::NotRemote))
    }

    /// Prüfung 1: Policy vorhanden und für genau diesen Pfad.
    fn matching_policy<'p>(
        &self,
        policy: Option<&'p RemotePolicy>,
    ) -> Result<&'p RemotePolicy, RemoteRefusal> {
        match policy {
            Some(policy) if policy.path == self.path => Ok(policy),
            _ => Err(RemoteRefusal::NotRemote),
        }
    }

    /// Prüfungen 1-7 ohne Logging (siehe [`Self::allows_remote_invocation`]).
    fn check_remote_invocation(
        &self,
        policy: Option<&RemotePolicy>,
        grant: &RemoteGrant,
        enabled: RemoteSlice,
        raw_args: &[String],
    ) -> Result<&'static RemoteVerb, RemoteRefusal> {
        let policy = self.matching_policy(policy)?;
        if let Some(token) = raw_args
            .iter()
            .find(|t| policy.denied_tokens.contains(&t.as_str()))
        {
            return Err(RemoteRefusal::DeniedToken {
                token: token.clone(),
            });
        }
        let verb = policy
            .find_verb(raw_args)
            .ok_or(RemoteRefusal::VerbNotRemote)?;
        check_remote_verb(verb, grant, enabled)?;
        Ok(verb)
    }

    /// Gibt den maschinellen Namen der zugrundeliegenden Operation zurück.
    ///
    /// # Beschreibung
    /// Liefert [`OperationMeta::name`] (z. B. `"session.list"`). Der Wert ist
    /// identisch mit `self.operation().meta().name`, aber ohne zusätzlichen
    /// Trait-Dispatch.
    ///
    /// # Returns
    /// `&str` — der maschinelle Op-Name. Lebensdauer an `&self` (`'static` intern).
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // assert_eq!(adapter.operation_name(), "session.list");
    /// ```
    #[must_use]
    pub fn operation_name(&self) -> &str {
        self.operation_name
    }

    /// Führt die Operation mit rohen `/command`-Argumenten aus.
    ///
    /// # Beschreibung
    /// Baut `OpInput { raw_args, json_args: serde_json::Value::Null }` und
    /// delegiert den Aufruf vollständig an `Operation::run`. Es findet kein
    /// Rendering und keine Argument-Transformation statt — der zurückgegebene
    /// [`OpOutput`] ist das direkte Ergebnis der Operation.
    ///
    /// # Arguments
    /// - `ctx` (`&OpContext`): Ausführungs-Kontext (Session, Turn, Sandbox, Services).
    ///   Wird unverändert an `Operation::run` weitergegeben.
    /// - `raw_args` (`Vec<String>`): Rohe Argumenttoken aus der Command-Zeile,
    ///   z. B. `["--limit", "10"]`. Darf leer sein.
    ///
    /// # Returns
    /// - `Ok(OpOutput)`: Die Operation wurde erfolgreich ausgeführt.
    /// - `Err(OpError)`: Fehler, der 1:1 von `Operation::run` stammt.
    ///
    /// # Errors
    /// - [`OpError::InvalidArguments`]: `raw_args` sind syntaktisch oder semantisch ungültig
    ///   (gemeldet von der Op).
    /// - [`OpError::Execution`]: Laufzeitfehler während der Operationsausführung.
    /// - [`OpError::NotAvailable`]: Operation im aktuellen Kontext nicht verfügbar.
    ///
    /// # Concurrency
    /// Async, `Send`-fähig. Der Adapter selbst hat keinen veränderlichen Zustand;
    /// mehrere gleichzeitige `dispatch`-Aufrufe auf demselben Adapter sind sicher.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // let out = adapter.dispatch(&ctx, vec!["--limit".to_owned(), "5".to_owned()]).await?;
    /// // println!("{}", out.text);
    /// ```
    pub async fn dispatch(
        &self,
        ctx: &OpContext,
        raw_args: Vec<String>,
    ) -> Result<OpOutput, OpError> {
        use tracing::Instrument;
        let span = tracing::info_span!(
            "operation.command",
            op = self.operation_name,
            path = self.path,
            arg_count = raw_args.len()
        );
        async move {
            let start = std::time::Instant::now();
            let input = OpInput::command(self.path, raw_args);
            let result = self.op.run(ctx, input).await;
            tracing::info!(
                duration_ms = start.elapsed().as_millis() as u64,
                status = if result.is_ok() { "ok" } else { "err" },
                "operation.command.done"
            );
            result
        }
        .instrument(span)
        .await
    }

    /// Gibt eine Referenz auf die zugrundeliegende Operation zurück.
    ///
    /// # Beschreibung
    /// Ermöglicht Introspektion oder Meta-Zugriff auf die Op, ohne den Adapter zu
    /// konsumieren. Das `Arc` wird nicht geklont — die Referenz ist an `&self` gebunden.
    ///
    /// # Returns
    /// `&Arc<dyn Operation>` — Referenz auf den geteilten Op-Pointer.
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // let name = adapter.operation().meta().name;
    /// ```
    #[must_use]
    pub fn operation(&self) -> &Arc<dyn Operation> {
        &self.op
    }
}

/// Prüfungen 4-6 des Remote-Gates für ein einzelnes Verb: Scheibe, Cap, Stufe.
///
/// Die Cap kommt vor der Stufe, damit ein Beobachter-Gerät „braucht Recht Steuern"
/// sieht und nicht eine Stufen-Meldung.
fn check_remote_verb(
    verb: &RemoteVerb,
    grant: &RemoteGrant,
    enabled: RemoteSlice,
) -> Result<(), RemoteRefusal> {
    if verb.slice > enabled {
        return Err(RemoteRefusal::SliceDisabled { slice: verb.slice });
    }
    if !grant.has(verb.cap) {
        return Err(RemoteRefusal::MissingCap { cap: verb.cap });
    }
    if grant.tier < verb.tier {
        return Err(RemoteRefusal::TierTooLow { need: verb.tier });
    }
    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, OnceLock};

    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};

    use super::CommandAdapter;
    use crate::context::{OpContext, ServiceMap};
    use crate::error::OpError;
    use crate::operation::{
        ApprovalPolicy, BusyAvailability, CommandVisibility, OpFuture, OpInput, OpOutput,
        Operation, OperationCategory, OperationDomain, OperationMeta, PermissionTier, RemoteCap,
        RemoteEffect, RemoteGrant, RemotePolicy, RemoteRefusal, RemoteSlice, RemoteVerb, Surface,
    };
    use crate::test_support::{TestError, TestResult, ctx};

    // ── test helpers ─────────────────────────────────────────────────────────

    /// Erstellt einen minimalen [`OpContext`] für Tests.
    ///
    /// Verwendet einen atomaren Zähler für Thread-sichere, eindeutige
    /// Verzeichnisnamen, sodass parallele Tests nicht kollidieren.
    fn make_test_ctx() -> TestResult<(OpContext, PathBuf)> {
        static CTX_COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = CTX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "harw-cmd-adapter-test-{}-{}",
            std::process::id(),
            id
        ));
        std::fs::create_dir_all(tmp.join("ws"))
            .map_err(ctx("Test-Workspace-Verzeichnis anlegen"))?;
        let registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("WorkspaceRegistry bauen"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("Workspace auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new());
        Ok((ctx, tmp))
    }

    // ── test-op fixtures ──────────────────────────────────────────────────────

    /// Op ohne jegliche Surface-Deklaration.
    struct NoSurfaceOp;

    impl Operation for NoSurfaceOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "no-surface",
                summary: "Keine Flächen.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput::from("noop".to_owned())) })
        }
    }

    /// Op mit genau einer `Surface::Command`-Fläche.
    struct SingleCommandOp;

    impl Operation for SingleCommandOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "single-command",
                summary: "Ein Command.",
                domain: OperationDomain::Session,
                permission: PermissionTier::Operator,
                surfaces: vec![Surface::Command {
                    path: "/single",
                    visibility: CommandVisibility::TuiOnly,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput::from("single".to_owned())) })
        }
    }

    /// Op mit zwei `Surface::Command`-Flächen (verschiedene Pfade und Visibilities).
    struct TwoCommandOp;

    impl Operation for TwoCommandOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "two-command",
                summary: "Zwei Commands.",
                domain: OperationDomain::Agents,
                permission: PermissionTier::Maintainer,
                surfaces: vec![
                    Surface::Command {
                        path: "/alpha",
                        visibility: CommandVisibility::TuiOnly,
                    },
                    Surface::Command {
                        path: "/beta",
                        visibility: CommandVisibility::ChannelReduced,
                    },
                ],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput::from("two".to_owned())) })
        }

        fn channel_subcommands(&self) -> &'static [&'static str] {
            &["-", "show"]
        }
    }

    /// Op mit allen drei Command-Sichtbarkeiten und gesperrten Nebenverben.
    ///
    /// Erlaubt auf `/denied-reduced` nur `review`, sperrt aber `accept` und
    /// `reject` an jeder Argumentposition.
    struct DeniedTokensOp;

    impl Operation for DeniedTokensOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "denied-tokens",
                summary: "Command mit gesperrten Nebenverben.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Operator,
                surfaces: vec![
                    Surface::Command {
                        path: "/denied-tui",
                        visibility: CommandVisibility::TuiOnly,
                    },
                    Surface::Command {
                        path: "/denied-parity",
                        visibility: CommandVisibility::ChannelParity,
                    },
                    Surface::Command {
                        path: "/denied-reduced",
                        visibility: CommandVisibility::ChannelReduced,
                    },
                ],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput::from("denied".to_owned())) })
        }

        fn channel_subcommands(&self) -> &'static [&'static str] {
            &["review"]
        }

        fn channel_denied_tokens(&self) -> &'static [&'static str] {
            &["accept", "reject"]
        }
    }

    /// Op mit `ChannelReduced`-Fläche, Wildcard-Freigabe und Sperrliste.
    struct WildcardDeniedOp;

    impl Operation for WildcardDeniedOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "wildcard-denied",
                summary: "Wildcard-Command mit gesperrtem Nebenverb.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Operator,
                surfaces: vec![Surface::Command {
                    path: "/wildcard",
                    visibility: CommandVisibility::ChannelReduced,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput::from("wildcard".to_owned())) })
        }

        fn channel_subcommands(&self) -> &'static [&'static str] {
            &["*"]
        }

        fn channel_denied_tokens(&self) -> &'static [&'static str] {
            &["accept"]
        }
    }

    /// Op mit gemischten Surfaces: ein Command und ein ModelTool.
    struct MixedSurfaceOp;

    impl Operation for MixedSurfaceOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "mixed-surface",
                summary: "Command + ModelTool.",
                domain: OperationDomain::Execution,
                permission: PermissionTier::Owner,
                surfaces: vec![
                    Surface::Command {
                        path: "/mixed",
                        visibility: CommandVisibility::ChannelParity,
                    },
                    Surface::ModelTool {
                        readonly: true,
                        approval: ApprovalPolicy::None,
                    },
                ],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async { Ok(OpOutput::from("mixed".to_owned())) })
        }
    }

    /// Op, die `input.raw_args.len()` als Textantwort zurückgibt.
    struct ArgCountOp;

    impl Operation for ArgCountOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "arg-count",
                summary: "Gibt die Anzahl der raw_args zurück.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![Surface::Command {
                    path: "/argcount",
                    visibility: CommandVisibility::TuiOnly,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, input: OpInput) -> OpFuture<'a> {
            Box::pin(async move {
                Ok(OpOutput::from(format!(
                    "{}",
                    input.invocation.raw_args().len()
                )))
            })
        }
    }

    /// Op, die stets `OpError::InvalidArguments` zurückgibt.
    struct InvalidArgsOp;

    impl Operation for InvalidArgsOp {
        fn meta(&self) -> &OperationMeta {
            static META: OnceLock<OperationMeta> = OnceLock::new();
            META.get_or_init(|| OperationMeta {
                name: "invalid-args",
                summary: "Schlägt immer mit InvalidArguments fehl.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![Surface::Command {
                    path: "/invalid",
                    visibility: CommandVisibility::TuiOnly,
                }],
                aliases: &[],
                category: OperationCategory::Misc,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
            Box::pin(async {
                Err(OpError::InvalidArguments(
                    "kein Argument erlaubt".to_owned(),
                ))
            })
        }
    }

    // ── from_operation tests ──────────────────────────────────────────────────

    #[test]
    fn test_from_operation_no_command_surface_returns_empty_vec() {
        let op: Arc<dyn Operation> = Arc::new(NoSurfaceOp);
        let adapters = CommandAdapter::from_operation(op);
        assert!(
            adapters.is_empty(),
            "Op ohne Surface::Command muss leeren Vec liefern"
        );
    }

    #[test]
    fn test_from_operation_single_command_surface_returns_one_adapter_with_correct_path() {
        let op: Arc<dyn Operation> = Arc::new(SingleCommandOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(adapters.len(), 1, "Genau ein Adapter erwartet");
        assert_eq!(adapters[0].path(), "/single");
    }

    #[test]
    fn test_from_operation_two_command_surfaces_returns_two_adapters_in_order() {
        let op: Arc<dyn Operation> = Arc::new(TwoCommandOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(adapters.len(), 2, "Zwei Adapter erwartet");
        assert_eq!(
            adapters[0].path(),
            "/alpha",
            "Erster Adapter muss /alpha sein"
        );
        assert_eq!(
            adapters[1].path(),
            "/beta",
            "Zweiter Adapter muss /beta sein"
        );
    }

    #[test]
    fn test_from_operation_two_command_surfaces_visibilities_match_declaration_order() {
        let op: Arc<dyn Operation> = Arc::new(TwoCommandOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(adapters[0].visibility(), CommandVisibility::TuiOnly);
        assert_eq!(adapters[1].visibility(), CommandVisibility::ChannelReduced);
    }

    #[test]
    fn test_from_operation_mixed_surfaces_returns_only_command_adapters() {
        let op: Arc<dyn Operation> = Arc::new(MixedSurfaceOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(
            adapters.len(),
            1,
            "Nur der Command-Eintrag soll einen Adapter erzeugen; ModelTool wird ignoriert"
        );
        assert_eq!(adapters[0].path(), "/mixed");
    }

    // ── channel policy tests ─────────────────────────────────────────────────

    #[test]
    fn test_channel_policy_tui_only_is_always_rejected() {
        let adapters = CommandAdapter::from_operation(Arc::new(TwoCommandOp));
        assert!(!adapters[0].allows_channel_invocation(&[]));
        assert!(!adapters[0].allows_channel_invocation(&["show".to_owned()]));
    }

    #[test]
    fn test_channel_policy_parity_accepts_arbitrary_args() {
        let adapters = CommandAdapter::from_operation(Arc::new(MixedSurfaceOp));
        assert!(adapters[0].allows_channel_invocation(&[]));
        assert!(adapters[0].allows_channel_invocation(&["anything".to_owned()]));
    }

    #[test]
    fn test_channel_policy_reduced_is_fail_closed_to_declared_subcommands() {
        let adapters = CommandAdapter::from_operation(Arc::new(TwoCommandOp));
        let reduced = &adapters[1];
        assert!(reduced.allows_channel_invocation(&[]));
        assert!(reduced.allows_channel_invocation(&["show".to_owned()]));
        assert!(!reduced.allows_channel_invocation(&["run".to_owned()]));
    }

    #[test]
    fn channel_reduced_refuses_denied_nested_verb() {
        let adapters = CommandAdapter::from_operation(Arc::new(DeniedTokensOp));
        let reduced = &adapters[2];
        assert_eq!(reduced.visibility(), CommandVisibility::ChannelReduced);
        assert!(
            !reduced.allows_channel_invocation(&[
                "review".to_owned(),
                "x".to_owned(),
                "accept".to_owned(),
                "p".to_owned(),
            ]),
            "gesperrtes Nebenverb hinter erlaubtem Erst-Token muss abgelehnt werden"
        );
        assert!(
            reduced.allows_channel_invocation(&["review".to_owned(), "x".to_owned()]),
            "erlaubte Form ohne gesperrtes Token muss zulässig bleiben"
        );
        assert!(
            !reduced.allows_channel_invocation(&["accept".to_owned()]),
            "gesperrtes Token als Erst-Token muss abgelehnt werden"
        );
    }

    #[test]
    fn channel_reduced_wildcard_still_honours_denied_tokens() {
        let adapters = CommandAdapter::from_operation(Arc::new(WildcardDeniedOp));
        let reduced = &adapters[0];
        assert_eq!(reduced.visibility(), CommandVisibility::ChannelReduced);
        assert!(
            reduced.allows_channel_invocation(&["list".to_owned()]),
            "Wildcard muss beliebige nicht gesperrte Formen freigeben"
        );
        assert!(
            !reduced.allows_channel_invocation(&[
                "review".to_owned(),
                "1".to_owned(),
                "accept".to_owned(),
                "2".to_owned(),
            ]),
            "Wildcard darf die Sperrliste nicht umgehen"
        );
    }

    #[test]
    fn tui_only_and_parity_are_unaffected_by_denied_tokens() {
        let adapters = CommandAdapter::from_operation(Arc::new(DeniedTokensOp));
        let tui_only = &adapters[0];
        let parity = &adapters[1];
        assert_eq!(tui_only.visibility(), CommandVisibility::TuiOnly);
        assert_eq!(parity.visibility(), CommandVisibility::ChannelParity);
        let clean = vec!["review".to_owned(), "x".to_owned()];
        let with_denied = vec!["review".to_owned(), "x".to_owned(), "accept".to_owned()];
        assert!(!tui_only.allows_channel_invocation(&clean));
        assert!(!tui_only.allows_channel_invocation(&with_denied));
        assert!(parity.allows_channel_invocation(&clean));
        assert!(
            parity.allows_channel_invocation(&with_denied),
            "ChannelParity übernimmt die volle Grammatik, auch mit gesperrten Tokens"
        );
    }

    // ── dispatch tests ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_dispatch_passes_raw_args_to_operation() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(ArgCountOp);
        let adapters = CommandAdapter::from_operation(Arc::clone(&op));
        let (ctx, tmp) = make_test_ctx()?;
        let args = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let result = adapters[0].dispatch(&ctx, args).await;
        std::fs::remove_dir_all(tmp).ok();
        match result {
            Ok(out) => assert_eq!(out.text, "3", "raw_args.len() soll 3 sein"),
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_dispatch_passes_empty_raw_args() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(ArgCountOp);
        let adapters = CommandAdapter::from_operation(Arc::clone(&op));
        let (ctx, tmp) = make_test_ctx()?;
        let result = adapters[0].dispatch(&ctx, vec![]).await;
        std::fs::remove_dir_all(tmp).ok();
        match result {
            Ok(out) => assert_eq!(out.text, "0"),
            Err(e) => return Err(TestError::Unexpected(format!("Unerwarteter Fehler: {e}"))),
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_dispatch_propagates_invalid_arguments_error() -> TestResult {
        let op: Arc<dyn Operation> = Arc::new(InvalidArgsOp);
        let adapters = CommandAdapter::from_operation(Arc::clone(&op));
        let (ctx, tmp) = make_test_ctx()?;
        let result = adapters[0].dispatch(&ctx, vec![]).await;
        std::fs::remove_dir_all(tmp).ok();
        match result {
            Err(OpError::InvalidArguments(_)) => {}
            other => {
                return Err(TestError::Unexpected(format!(
                    "Erwartet OpError::InvalidArguments, war: {other:?}"
                )));
            }
        }
        Ok(())
    }

    // ── accessor tests ────────────────────────────────────────────────────────

    #[test]
    fn test_permission_returns_operation_meta_permission() {
        let op: Arc<dyn Operation> = Arc::new(SingleCommandOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(
            adapters[0].permission(),
            PermissionTier::Operator,
            "permission() muss OperationMeta::permission widerspiegeln"
        );
    }

    #[test]
    fn test_permission_owner_tier_is_propagated() {
        let op: Arc<dyn Operation> = Arc::new(MixedSurfaceOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(adapters[0].permission(), PermissionTier::Owner);
    }

    #[test]
    fn test_visibility_returns_surface_command_visibility() {
        let op: Arc<dyn Operation> = Arc::new(SingleCommandOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(
            adapters[0].visibility(),
            CommandVisibility::TuiOnly,
            "visibility() muss den Wert aus Surface::Command liefern"
        );
    }

    #[test]
    fn test_visibility_channel_parity_is_propagated() {
        let op: Arc<dyn Operation> = Arc::new(MixedSurfaceOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(adapters[0].visibility(), CommandVisibility::ChannelParity);
    }

    #[test]
    fn test_operation_name_returns_meta_name() {
        let op: Arc<dyn Operation> = Arc::new(SingleCommandOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(
            adapters[0].operation_name(),
            "single-command",
            "operation_name() muss OperationMeta::name liefern"
        );
    }

    #[test]
    fn test_operation_name_consistent_across_multiple_adapters_of_same_op() {
        let op: Arc<dyn Operation> = Arc::new(TwoCommandOp);
        let adapters = CommandAdapter::from_operation(op);
        assert_eq!(adapters[0].operation_name(), "two-command");
        assert_eq!(adapters[1].operation_name(), "two-command");
    }

    #[test]
    fn test_operation_returns_arc_with_correct_meta_name() {
        let op: Arc<dyn Operation> = Arc::new(SingleCommandOp);
        let adapters = CommandAdapter::from_operation(Arc::clone(&op));
        assert_eq!(adapters[0].operation().meta().name, "single-command");
    }

    // ── Send + Sync compile-time check ────────────────────────────────────────

    #[test]
    fn test_command_adapter_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CommandAdapter>();
    }

    // ── remote gate tests ─────────────────────────────────────────────────────

    /// Adapter `/single` (`TuiOnly`, Operator).
    fn tui_only_adapter() -> CommandAdapter {
        let mut adapters = CommandAdapter::from_operation(Arc::new(SingleCommandOp));
        assert_eq!(adapters.len(), 1);
        let adapter = adapters.remove(0);
        assert_eq!(adapter.visibility(), CommandVisibility::TuiOnly);
        adapter
    }

    /// Adapter `/mixed` (`ChannelParity`, Owner).
    fn parity_adapter() -> CommandAdapter {
        let mut adapters = CommandAdapter::from_operation(Arc::new(MixedSurfaceOp));
        assert_eq!(adapters.len(), 1);
        let adapter = adapters.remove(0);
        assert_eq!(adapter.visibility(), CommandVisibility::ChannelParity);
        adapter
    }

    fn args(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|t| (*t).to_owned()).collect()
    }

    /// Nur Beobachten, Stufe Observer.
    const OBSERVER: RemoteGrant = RemoteGrant {
        observe: true,
        steer: false,
        control: false,
        tier: PermissionTier::Observer,
    };

    /// Beobachten und Steuern, Stufe Operator, ohne Control.
    const OPERATOR_NO_CONTROL: RemoteGrant = RemoteGrant {
        observe: true,
        steer: true,
        control: false,
        tier: PermissionTier::Operator,
    };

    /// Alle Caps, Stufe Owner.
    const FULL: RemoteGrant = RemoteGrant {
        observe: true,
        steer: true,
        control: true,
        tier: PermissionTier::Owner,
    };

    static SINGLE_SHOW: RemotePolicy = RemotePolicy {
        path: "/single",
        verbs: &[RemoteVerb::read(&["show", "-"])],
        denied_tokens: &[],
    };

    static SINGLE_STEER: RemotePolicy = RemotePolicy {
        path: "/single",
        verbs: &[RemoteVerb::steer(&["*"], RemoteEffect::Applied)],
        denied_tokens: &[],
    };

    static SINGLE_CONTROL: RemotePolicy = RemotePolicy {
        path: "/single",
        verbs: &[RemoteVerb::control(&["*"], RemoteEffect::Applied)],
        denied_tokens: &[],
    };

    static SINGLE_GLOBAL_SET: RemotePolicy = RemotePolicy {
        path: "/single",
        verbs: &[RemoteVerb::global(&["set", "*"], RemoteEffect::Staged)],
        denied_tokens: &[],
    };

    static SINGLE_READ_THEN_STOP: RemotePolicy = RemotePolicy {
        path: "/single",
        verbs: &[
            RemoteVerb::read(&["-"]),
            RemoteVerb::steer(&["stop", "*"], RemoteEffect::Applied),
        ],
        denied_tokens: &[],
    };

    static SINGLE_DENIED: RemotePolicy = RemotePolicy {
        path: "/single",
        verbs: &[RemoteVerb::read(&["*"])],
        denied_tokens: &["--allow-insecure-lan"],
    };

    static SINGLE_EMPTY: RemotePolicy = RemotePolicy {
        path: "/single",
        verbs: &[],
        denied_tokens: &[],
    };

    static OTHER_PATH: RemotePolicy = RemotePolicy {
        path: "/other",
        verbs: &[RemoteVerb::read(&["*"])],
        denied_tokens: &[],
    };

    static MIXED_READ: RemotePolicy = RemotePolicy {
        path: "/mixed",
        verbs: &[RemoteVerb::read(&["*"])],
        denied_tokens: &[],
    };

    #[test]
    fn remote_without_policy_is_refused() {
        let tui = tui_only_adapter();
        assert_eq!(
            tui.allows_remote_invocation(None, &FULL, RemoteSlice::Global, &[]),
            Err(RemoteRefusal::NotRemote)
        );
        let parity = parity_adapter();
        assert_eq!(
            parity.allows_remote_invocation(None, &FULL, RemoteSlice::Global, &args(&["x"])),
            Err(RemoteRefusal::NotRemote),
            "ChannelParity ist ohne Policy nicht remote (eigene Achse, fail-closed)"
        );
        assert_eq!(
            parity.remote_availability(None, &FULL, RemoteSlice::Global),
            Err(RemoteRefusal::NotRemote)
        );
    }

    #[test]
    fn remote_policy_path_must_match() {
        let tui = tui_only_adapter();
        assert_eq!(
            tui.allows_remote_invocation(Some(&OTHER_PATH), &FULL, RemoteSlice::Global, &[]),
            Err(RemoteRefusal::NotRemote)
        );
        assert_eq!(
            tui.remote_availability(Some(&OTHER_PATH), &FULL, RemoteSlice::Global),
            Err(RemoteRefusal::NotRemote)
        );
        // Gegenprobe: dieselbe Form-Policy unter dem richtigen Pfad greift.
        let parity = parity_adapter();
        assert!(
            parity
                .allows_remote_invocation(Some(&MIXED_READ), &OBSERVER, RemoteSlice::Read, &[])
                .is_ok()
        );
    }

    #[test]
    fn remote_tui_only_with_policy_runs_read_form() -> TestResult {
        let tui = tui_only_adapter();
        let verb = tui
            .allows_remote_invocation(
                Some(&SINGLE_SHOW),
                &OBSERVER,
                RemoteSlice::Read,
                &args(&["show"]),
            )
            .map_err(|r| TestError::Unexpected(format!("unerwartete Ablehnung: {r:?}")))?;
        assert_eq!(verb.cap, RemoteCap::Observe);
        assert!(
            std::ptr::eq(verb, &SINGLE_SHOW.verbs[0]),
            "das 'static Verb der Policy wird zurückgegeben"
        );
        // Kanal-Gate bleibt davon unberührt.
        assert!(!tui.allows_channel_invocation(&args(&["show"])));
        // Andere Form derselben Policy: nicht remote.
        assert_eq!(
            tui.allows_remote_invocation(
                Some(&SINGLE_SHOW),
                &OBSERVER,
                RemoteSlice::Read,
                &args(&["show", "x"]),
            ),
            Err(RemoteRefusal::VerbNotRemote)
        );
        assert_eq!(
            tui.allows_remote_invocation(Some(&SINGLE_SHOW), &OBSERVER, RemoteSlice::Read, &[]),
            Err(RemoteRefusal::VerbNotRemote)
        );
        Ok(())
    }

    #[test]
    fn observer_cannot_steer_or_control() {
        let tui = tui_only_adapter();
        let a = args(&["stop"]);
        assert_eq!(
            tui.allows_remote_invocation(Some(&SINGLE_STEER), &OBSERVER, RemoteSlice::Global, &a),
            Err(RemoteRefusal::MissingCap {
                cap: RemoteCap::Steer
            })
        );
        assert_eq!(
            tui.allows_remote_invocation(Some(&SINGLE_CONTROL), &OBSERVER, RemoteSlice::Global, &a),
            Err(RemoteRefusal::MissingCap {
                cap: RemoteCap::Control
            })
        );
    }

    #[test]
    fn operator_without_control_cannot_manage() {
        let tui = tui_only_adapter();
        assert_eq!(
            tui.allows_remote_invocation(
                Some(&SINGLE_GLOBAL_SET),
                &OPERATOR_NO_CONTROL,
                RemoteSlice::Global,
                &args(&["set", "theme", "dark"]),
            ),
            Err(RemoteRefusal::MissingCap {
                cap: RemoteCap::Control
            })
        );
        // Gegenprobe: mit Control läuft die Form; Step-up meldet das Verb, prüft der Aufrufer.
        let verb = tui.allows_remote_invocation(
            Some(&SINGLE_GLOBAL_SET),
            &FULL,
            RemoteSlice::Global,
            &args(&["set", "theme", "dark"]),
        );
        assert!(matches!(verb, Ok(v) if v.stepup));
    }

    #[test]
    fn tier_is_checked_after_caps() {
        let tui = tui_only_adapter();
        let all_caps_observer = RemoteGrant {
            tier: PermissionTier::Observer,
            ..FULL
        };
        assert_eq!(
            tui.allows_remote_invocation(
                Some(&SINGLE_STEER),
                &all_caps_observer,
                RemoteSlice::Global,
                &args(&["stop"]),
            ),
            Err(RemoteRefusal::TierTooLow {
                need: PermissionTier::Operator
            })
        );
        // Fehlen Cap und Stufe zugleich, gewinnt die Cap.
        assert_eq!(
            tui.allows_remote_invocation(
                Some(&SINGLE_STEER),
                &OBSERVER,
                RemoteSlice::Global,
                &args(&["stop"]),
            ),
            Err(RemoteRefusal::MissingCap {
                cap: RemoteCap::Steer
            })
        );
    }

    #[test]
    fn slice_gate() {
        let tui = tui_only_adapter();
        assert_eq!(
            tui.allows_remote_invocation(
                Some(&SINGLE_GLOBAL_SET),
                &FULL,
                RemoteSlice::Session,
                &args(&["set", "x"]),
            ),
            Err(RemoteRefusal::SliceDisabled {
                slice: RemoteSlice::Global
            })
        );
        assert_eq!(
            tui.allows_remote_invocation(
                Some(&SINGLE_STEER),
                &FULL,
                RemoteSlice::Read,
                &args(&["stop"]),
            ),
            Err(RemoteRefusal::SliceDisabled {
                slice: RemoteSlice::Session
            })
        );
        // Scheibe vor Cap: auch ein Beobachter sieht zuerst die Scheibe.
        assert_eq!(
            tui.allows_remote_invocation(
                Some(&SINGLE_GLOBAL_SET),
                &OBSERVER,
                RemoteSlice::Session,
                &args(&["set", "x"]),
            ),
            Err(RemoteRefusal::SliceDisabled {
                slice: RemoteSlice::Global
            })
        );
    }

    #[test]
    fn denied_token_anywhere() {
        let tui = tui_only_adapter();
        let token = "--allow-insecure-lan";
        for a in [
            args(&["scan", "p", token]),
            args(&[token]),
            args(&["scan", token, "p"]),
        ] {
            assert_eq!(
                tui.allows_remote_invocation(Some(&SINGLE_DENIED), &FULL, RemoteSlice::Global, &a),
                Err(RemoteRefusal::DeniedToken {
                    token: token.to_owned()
                })
            );
        }
        assert!(
            tui.allows_remote_invocation(
                Some(&SINGLE_DENIED),
                &FULL,
                RemoteSlice::Global,
                &args(&["scan", "p"]),
            )
            .is_ok()
        );
    }

    #[test]
    fn remote_availability_reports_first_refusal() {
        let tui = tui_only_adapter();
        assert_eq!(
            tui.remote_availability(Some(&SINGLE_STEER), &OBSERVER, RemoteSlice::Global),
            Err(RemoteRefusal::MissingCap {
                cap: RemoteCap::Steer
            })
        );
        assert_eq!(
            tui.remote_availability(Some(&SINGLE_READ_THEN_STOP), &OBSERVER, RemoteSlice::Global),
            Ok(())
        );
        // Mehrere Ablehnungen: die des ersten Verbs gewinnt (Scheibe vor Cap).
        assert_eq!(
            tui.remote_availability(Some(&SINGLE_READ_THEN_STOP), &OBSERVER, RemoteSlice::Read),
            Ok(())
        );
        static STEER_THEN_GLOBAL: RemotePolicy = RemotePolicy {
            path: "/single",
            verbs: &[
                RemoteVerb::steer(&["stop", "*"], RemoteEffect::Applied),
                RemoteVerb::global(&["set", "*"], RemoteEffect::Staged),
            ],
            denied_tokens: &[],
        };
        assert_eq!(
            tui.remote_availability(Some(&STEER_THEN_GLOBAL), &OBSERVER, RemoteSlice::Session),
            Err(RemoteRefusal::MissingCap {
                cap: RemoteCap::Steer
            })
        );
        // Leere Verbliste: nicht remote.
        assert_eq!(
            tui.remote_availability(Some(&SINGLE_EMPTY), &FULL, RemoteSlice::Global),
            Err(RemoteRefusal::NotRemote)
        );
    }

    #[test]
    fn remote_gate_order_is_policy_then_denied_then_verb() {
        let tui = tui_only_adapter();
        // Gesperrtes Token vor unbekannter Form.
        static DENIED_NO_VERB: RemotePolicy = RemotePolicy {
            path: "/single",
            verbs: &[RemoteVerb::read(&["show", "-"])],
            denied_tokens: &["--force"],
        };
        assert_eq!(
            tui.allows_remote_invocation(
                Some(&DENIED_NO_VERB),
                &FULL,
                RemoteSlice::Global,
                &args(&["wipe", "--force"]),
            ),
            Err(RemoteRefusal::DeniedToken {
                token: "--force".to_owned()
            })
        );
        // Unbekannte Form vor Scheibe/Cap/Stufe.
        assert_eq!(
            tui.allows_remote_invocation(
                Some(&DENIED_NO_VERB),
                &OBSERVER,
                RemoteSlice::Read,
                &args(&["wipe"]),
            ),
            Err(RemoteRefusal::VerbNotRemote)
        );
    }

    #[test]
    fn channel_gate_is_unchanged_by_remote_axis() {
        // Kanal-Ergebnisse wie vor der Remote-Achse (siehe channel policy tests oben).
        let tui = tui_only_adapter();
        assert!(!tui.allows_channel_invocation(&[]));
        let parity = parity_adapter();
        assert!(parity.allows_channel_invocation(&args(&["anything"])));
    }
}

//! Turn-Loop: der eigentliche Agent-Zyklus.
//!
//! Schritt für Schritt:
//! 1. Context sammeln (ContextProvider)
//! 2. Instructions laden (InstructionsProvider)
//! 3. Observer benachrichtigen (TurnObserver::on_turn_start)
//! 4. Model Call (provider-neutral — noch nicht implementiert)
//! 5. Für jeden ToolCall:
//!    a. Guardrail (ApprovalHandler)
//!    b. Handoff? → AgentSpawner → WaitingForChild
//!    c. Sonst: ToolExecutor::execute
//!    d. ToolResult in History
//! 6. Keine ToolCalls mehr → Turn fertig
//! 7. Observer: on_turn_stop, state → Idle
//!
//! # Tracing
//!
//! Every public entry point (run_turn, run_turn_durable, resume_after_child,
//! resume_after_child_durable, resume_after_approval, resume_after_approval_durable)
//! is wrapped in an `agent.turn` span carrying `turn_id` and `session_id`.
//!
//! **Redaction policy**: no prompt text, no tool arguments JSON, no response
//! content is ever logged. Only IDs, names, byte-sizes, counters, and status
//! strings ("ok" | "err") appear in structured fields.
//!
//! Span/event hierarchy per turn:
//! ```text
//! agent.turn { turn_id, session_id }
//! ├─ model.request  { size_bytes }          ← info event
//! ├─ model.response { size_bytes, tool_call_count }   ← info event
//! ├─ tool.call      { tool_name }           ← info span
//! │    └─ tool.execute { duration_ms, status } ← info event
//! ├─ approval.wait  { approval_kind }       ← info span
//! └─ transcript.persist { records_written } ← debug event
//! ```
//!
//! # W4a A-LOOP: Abbruch, Grenzwerte, Vertrauen, Stop-Gründe
//!
//! - **Steuerblock je Turn.** [`TurnControl`] trägt den [`CancelToken`], die
//!   [`TurnLimits`], die Serveruhr ([`harw_types::Clock`]) und einen geteilten
//!   Zähler (Modellrunden, Werkzeugaufrufe, Token-Nutzung, Startzeit). Er reist
//!   in [`TurnInput::control`] in `run_turn`/`run_turn_durable` hinein; die
//!   `resume_*`-Einstiege bekommen **denselben** Block als `&TurnControl`, damit
//!   Grenzwerte und Nutzung über eine Pause hinweg weiterzählen.
//! - **Prüfpunkte.** Vor jedem Modellaufruf (Abbruch, Runden, Ausgabe-Tokens,
//!   Wanduhr) und vor jeder Werkzeugausführung (Abbruch, Aufrufzahl, Wanduhr).
//!   Der Modellaufruf selbst läuft gegen `CancelToken::cancelled` (kein Timer
//!   nötig). Ein Treffer beendet den Turn mit [`TurnOutcome::Cancelled`]
//!   (`CancelReason::Budget` bei einer Grenzwertverletzung); jeder noch offene
//!   Tool-Call der Runde bekommt ein synthetisches Fehlerergebnis mit
//!   `ResultTrust::Runtime`, damit der Verlauf provider-gültig bleibt.
//! - **Vertrauen.** Werkzeugausgaben landen mit `ResultTrust::Untrusted` im
//!   Verlauf; vom Harness erzeugte Ergebnisse (Ablehnung, Abbruch, fehlender
//!   Kontext, fehlender Ausführer, deaktivierter Handoff) mit
//!   `ResultTrust::Runtime`.
//! - **Stop-Gründe.** Bei `StopReason::MaxTokens` fordert der Loop höchstens
//!   drei Mal automatisch eine nahtlose Fortsetzung an. Bleibt die Ausgabe
//!   danach abgeschnitten, sowie sofort bei `ContextWindowExceeded`, endet der
//!   Turn mit [`TurnOutcome::Truncated`]. `StopReason::{Refusal,
//!   ContentFilter}` endet mit [`TurnOutcome::Refused`]. Tool-Calls einer
//!   solchen Antwort werden nie ausgeführt (Argumente können abgeschnitten
//!   sein). Nur für die UIA-Root-Session (kein Parent, Organisationsrolle
//!   `UserInterface` — Kriterium wie `is_uia_root_session` in
//!   `harw-tui/src/session_controller.rs`): trägt `response.reasoning`
//!   mindestens einen Anthropic-`"thinking"`-Block mit lesbarem Text, wird
//!   dieser Text als `TurnItem::Reasoning` (VOR der AssistantMessage) in die
//!   History gepusht und per `TurnEvent::ItemAdded` gemeldet.
//!   `"redacted_thinking"`- und verschlüsseltes OpenAI-Reasoning liefern
//!   keinen extrahierbaren Text und bleiben unsichtbar; alle anderen
//!   Sessions sehen nie ein `TurnItem::Reasoning`. `to_model_messages`
//!   (`history.rs`) überspringt `Reasoning`-Items beim nächsten
//!   Provider-Request ohnehin — das Item stört dort also nicht.
//! - **Resume-Fehler.** Eine abgelehnte Wiederaufnahme (falscher Actor, falsches
//!   Kind, keine offene Anfrage, bereits aufgelöst) bleibt `Err` und lässt die
//!   Pause intakt. Scheitert eine *angenommene* Wiederaufnahme (Persistenz,
//!   Spawner, Ausführungsgrenze) oder ist die dauerhafte Freigabe abgelaufen
//!   bzw. defekt, endet der Turn mit [`TurnOutcome::Failed`] (Session `Failed`).
//! - **Handoffs** (`transfer_to_*`) laufen nur, wenn die Aktivierung der Session
//!   den Werkzeugnamen freigibt.
//!
//! # AW1-03: warum Schritt 1 noch der alte Pfad ist
//!
//! [`gather_context`] (Schritt 1) liefert bis heute
//! `Vec<harw_extension_api::ContextFragment>` — nur `label` + `content`,
//! ohne Sektion, `TrustClass`, `Stability` oder Kosten. `drive_turn` reicht
//! das unverändert an `ModelRequest::with_context_budget` weiter, das intern
//! die **Bestandsmontage** aus `crate::context_budget` (`ContextBudget`,
//! `ContextAssembly`, `assemble`) aufruft.
//!
//! Der neue, `ContextProgram`-bewusste, deterministisch geordnete Pfad aus
//! diesem Knoten — `context_budget::Assembly<Gathered|Admitted|Budgeted>`,
//! der `harw_context::Fragment` statt `ContextFragment` voraussetzt — ist
//! **absichtlich nicht** hier verdrahtet: es gibt noch keinen
//! `ContextProvider`, der ein `harw_context::Fragment` (mit Sektion,
//! Vertrauensklasse, Stabilität, bereits berechneten Kosten) produziert.
//! Eine Verdrahtung an dieser Stelle hätte bedeutet, diese fünf Angaben pro
//! Fragment zu erfinden — das wäre keine Durchsetzung gewesen, nur ihr
//! Anschein. Sobald ein Fragment-Provider existiert, ersetzt
//! `context_budget::Assembly::gather(fragments).admit(&program,
//! &ceiling)?.budget(&spec)?.render()` diesen Schritt.
//!
//! ## Nachtrag (Folgeknoten): eine der beiden Lücken hat sich seither geschlossen
//!
//! Eigene Prüfung von `crate::session`, nicht Übernahme des obigen Absatzes:
//! `SpawnContext` (`session.rs`) trägt inzwischen ein
//! `ceiling: Option<harw_context::ContextCeiling>`, erreichbar über
//! `session.spawn_context().and_then(|c| c.ceiling.clone())`. Eine
//! `ContextCeiling` ist damit produktiv vorhanden. **Kein** `ContextProgram`
//! ist es weiterhin: weder `AgentSession` noch `SpawnContext` tragen eines,
//! und `harw_agent_dsl::executable::ContextProgram` selbst hat (eigene
//! Prüfung von `harw-agent-dsl/src/executable.rs`) kein Feld für einen
//! `harw_context::DetailMode` je Sektion — nur `must_include`/`exclude`.
//! Der ursprüngliche Blocker "fünf Angaben pro Fragment erfinden" (Absatz
//! oben) bleibt deshalb unverändert bestehen: eine Ceiling allein liefert
//! weder Sektion noch Vertrauen noch Stabilität noch Kosten eines Fragments
//! — nur eine Erlaubnisgrenze dafür. Siehe `crate::context_budget`s
//! Modul-Abschnitt "Die nachgeholte Verdrahtung" für die vollständige,
//! aktuelle Bestandsaufnahme aller drei noch offenen Voraussetzungen
//! (Fragment-Provider, `ContextProgram` je Sitzung, ein Weg von hier zu
//! `harw_tools::context_load::ContextLoadExecutor::seed_turn`) sowie für die
//! beiden additiven Bausteine, die dieser Folgeknoten dafür bereits in
//! `context_budget.rs` bereitgestellt hat
//! (`ContextAssemblyV2::render_trust_blocks_with_detail`,
//! `ContextAssemblyV2::spent_per_section`) — beide ungenutzt von dieser
//! Datei, aus genau den hier genannten Gründen, nicht aus Versehen.
//!
//! ## Zweiter Nachtrag (dieser Knoten): `ContextProgram` ist jetzt erreichbar,
//! `DetailMode` auch — zwei der vier Lücken sind kleiner geworden, keine ist ganz zu
//!
//! Eigene Prüfung dieses Knotens gegen den obigen Absatz:
//!
//! 1. **`ContextProgram` je Sitzung ist jetzt erreichbar.**
//!    [`crate::session::AgentSession`] trägt seit diesem Knoten
//!    `context_program: Option<harw_agent_dsl::executable::ContextProgram>`
//!    über `AgentSession::with_context_program`/`AgentSession::context_program`
//!    (bewusst auf `AgentSession`, nicht auf [`SpawnContext`] — Begründung in
//!    `session.rs`s Moduldoku, Abschnitt "Folgeknoten zu AW2-01/AW2-02"). Eine
//!    Sitzung ohne diesen Aufruf verhält sich unverändert: `gather_context`
//!    unten liest dieses Feld nicht, also ändert seine bloße Existenz kein
//!    bestehendes Rendering.
//! 2. **`harw_agent_dsl::executable::ContextProgram` trägt jetzt den
//!    [`harw_context::DetailMode`] je Sektion.** `ContextProgram::section_detail`
//!    (Folgeknoten zu AW2-01) schließt genau die Lücke, die der Absatz oben
//!    ("kein Feld für einen `harw_context::DetailMode` je Sektion") noch offen
//!    ließ — `SNAPSHOT_HASH_DOMAIN` in `executable.rs` ist deshalb auf `v3`
//!    gestiegen.
//! 3. **`gather_context` (Schritt 1 unten) ist unverändert der alte Pfad.**
//!    Es liefert weiterhin `Vec<harw_extension_api::ContextFragment>`, nicht
//!    `harw_context::Fragment`. Diese Datei besitzt keinen `ContextProvider`
//!    — die Trait-Definition und ihre Implementierungen liegen in
//!    `harw-extension-api`/den jeweiligen Provider-Crates, außerhalb des
//!    Schreibbereichs dieses Knotens (`harw-agent-dsl/src/executable.rs`,
//!    `harw-core/src/session.rs`, `harw-core/src/turn_loop.rs`). Der
//!    ursprüngliche Blocker "kein `ContextProvider` erzeugt ein
//!    `harw_context::Fragment`" bleibt deshalb unverändert bestehen — **hier**
//!    bricht die Kette zu `FragmentReference`/`DetailMode::References`
//!    weiterhin, nicht erst bei der Montage.
//! 4. **Kein Weg von hier zu einer konkreten `ContextLoadExecutor`-Instanz.**
//!    [`find_executor`] liefert ausschließlich `Arc<dyn ToolExecutor>`
//!    (`harw_tools::executor::ToolExecutor`, definiert in
//!    `harw-tools/src/executor.rs:64`) — dieses Trait-Objekt bietet keine
//!    Möglichkeit, das konkrete `harw_tools::context_load::ContextLoadExecutor`
//!    dahinter zu erreichen. Geprüft und verworfen:
//!      - eine neue Methode auf `ToolExecutor` selbst (z. B.
//!        `fn as_context_load_executor(&self) -> Option<&ContextLoadExecutor> { None }`
//!        als Default-Methode in `harw-tools/src/executor.rs`, überschrieben
//!        in `impl ToolExecutor for ContextLoadExecutor`,
//!        `harw-tools/src/context_load.rs:539`) — berührt `harw-tools`, das
//!        nicht zum Schreibbereich dieses Knotens gehört;
//!      - ein `downcast` über `std::any::Any` in dieser Datei — technisch
//!        machbar (`ToolExecutor: Send + Sync` bräuchte zusätzlich
//!        `+ Any` bzw. eine `as_any`-Methode, ebenfalls eine Änderung in
//!        `harw-tools`), aber eine Typprüfung zur Laufzeit an einer Stelle,
//!        die sie mit der ersten Option nicht bräuchte — deshalb hier bewusst
//!        **nicht** eingebaut, um niemanden mit einem `downcast` zurückzulassen,
//!        den ein Folgeknoten erst wieder entfernen müsste.
//!
//!    Ein dritter, in diesem Schreibbereich baubarer Weg existiert nicht: jede
//!    Alternative läuft über die `Arc<dyn ToolExecutor>`-Grenze, die
//!    ausschließlich `harw-tools` definiert. Das ist der Befund für Punkt 3
//!    des Knotenauftrags — die obige Zeile ist die wörtliche Ergänzung, die in
//!    `harw-tools/src/executor.rs` nötig wäre.
//!
//! ## Dritter Nachtrag (dieser Knoten): Blocker 3 ist geschlossen — die Grenze
//! war der Schreibbereich, nicht die Machbarkeit
//!
//! `harw-tools/src/executor.rs` gehört jetzt zum Schreibbereich. Gebaut wurde
//! genau die Zeile, die Punkt 4 oben als Befund nannte:
//! `ToolExecutor::as_context_load_executor` als Default-Methode (`None`),
//! überschrieben in `impl ToolExecutor for ContextLoadExecutor`
//! (`harw-tools/src/context_load.rs`). Kein bestehender `ToolExecutor`
//! bricht dadurch — die Vorgabe ist dafür da. Kein `downcast` — aus demselben
//! Grund, den Punkt 4 oben bereits nannte.
//!
//! [`seed_context_load_ledger`] nutzt diesen Weg von einem echten
//! Eintrittspunkt aus: [`drive_turn`] ruft sie einmal je Turn auf, bevor die
//! Model-/Tool-Schleife beginnt. Sie erreicht **beide** Hälften — das
//! deklarierte `ContextProgram` über [`AgentSession::context_program`] und
//! die konkrete `ContextLoadExecutor`-Instanz über [`find_executor`] +
//! `as_context_load_executor` — und ruft `seed_turn` auf. Die dabei
//! übergebene `spent_per_section`-Map ist leer, weil Blocker 1 (kein
//! `ContextProvider` erzeugt `harw_context::Fragment`) unverändert offen ist:
//! ohne die neue Montage gibt es keine echten Kosten je Sektion zu
//! übergeben. **Hier, an genau dieser Stelle, bricht die Kette zu
//! `FragmentReference`/`DetailMode::References` jetzt weiterhin** — nicht
//! mehr am fehlenden Weg zum Ausführer.
//!
//! **Ergebnis (Stand vor diesem Knoten):** kein Produktionspfad erreicht
//! `FragmentReference` — Blocker 1 (`ContextProvider` → `harw_context::Fragment`)
//! lag außerhalb dieses Schreibbereichs.
//!
//! ## Vierter Nachtrag (dieser Knoten): Blocker 1 war eine Rückwandlung, kein
//! fehlender Erzeuger — die Montage läuft jetzt in Produktion
//!
//! Eigene Prüfung, dieser Knoten: **dieser** Knoten hat Schreibzugriff auf
//! `harw-core/src/model.rs` bekommen, den frühere Knoten nicht hatten (siehe
//! `context_budget.rs`s Moduldoku, Abschnitt „Warum der Bestandspfad
//! unangetastet bleibt" — dort ist die fehlende Berechtigung auf `model.rs`
//! als der Grund genannt, warum die Montage nicht angeschlossen wurde). Mit
//! diesem Zugriff zeigt sich: „Blocker 1" war nie ein fehlender
//! `harw_context::Fragment`-Erzeuger — [`gather_context`] ruft `contribute_v2`
//! bereits seit dem „Vierter Nachtrag" oben auf und bekommt dabei **echte**
//! `harw_context::Fragment`-Werte zurück (für jeden v1-Provider über
//! `harw_extension_api::fragment_from_v1`s dokumentierte, nicht erfundene
//! Abbildung: `trust = TrustClass::Data`, `stability = Stability::Fresh`,
//! `section` = eine feste v1-Sektion, `cost` über `BytesOverFour`). Der
//! Blocker war die **Rückwandlung**, die [`gather_context`] direkt danach auf
//! diese Fragmente anwandte, um `ModelRequest::with_context_budget`s
//! `Vec<ContextFragment>`-Signatur zu bedienen.
//!
//! [`gather_context`] liefert seit diesem Knoten `Vec<harw_context::Fragment>`
//! **ohne** diese Rückwandlung (siehe seine eigene Doku, „Fünfter Nachtrag").
//! [`drive_turn`] reicht das Ergebnis zusammen mit
//! [`AgentSession::context_program`] und
//! `AgentSession::spawn_context().and_then(|c| c.ceiling.as_ref())` an
//! [`crate::model::ModelRequest::with_context_program`] weiter
//! (`harw-core/src/model.rs`, Moduldoku „Zwei Wege zur Kontextmontage") —
//! **dort**, nicht hier, entscheidet sich, ob die AW1-03/AW4-01-Montage läuft
//! oder auf den alten Byte-Budget-Pfad zurückfällt. Diese Datei kennt die
//! Unterscheidung selbst nicht mehr; sie reicht beide Werte unbedingt durch.
//!
//! **Ergebnis:**
//! - Die Zwei-Block-Trennung nach Vertrauensklassen (AW4-01) läuft jetzt in
//!   Produktion, für jede Sitzung, die sowohl ein `ContextProgram` als auch
//!   eine geschnittene `ContextCeiling` trägt.
//! - Der Nullzähler `TRUST_BLOCK_VIOLATION` ist damit von [`drive_turn`] aus
//!   erreichbar — nicht mehr nur aus den Unit-Tests von `context_budget.rs`.
//! - `DetailMode::References`/`FragmentReference` sind ebenfalls erreichbar:
//!   `ModelRequest::with_context_program` liest
//!   `ContextProgram::section_detail()` (`model.rs`s `section_detail_map`)
//!   und reicht das Ergebnis an `render_trust_blocks_with_detail` weiter.
//! - **Weiterhin offen:** `seed_context_load_ledger` unten übergibt
//!   `seed_turn` weiterhin eine **leere** `spent_per_section`-Map, nicht das
//!   Ergebnis von `ContextAssemblyV2::spent_per_section()`. Der Grund ist
//!   jetzt ein struktureller, kein fehlender Baustein mehr:
//!   `seed_context_load_ledger` läuft **einmal**, vor der Schleife, bevor
//!   überhaupt ein `ModelRequest` gebaut wird — die Montage, die
//!   `spent_per_section` liefern würde, entsteht aber erst **innerhalb**
//!   dieser Schleife, in `with_context_program`. Sie vorzuziehen würde
//!   entweder die Montage doppelt laufen lassen (einmal zum Seeden, einmal
//!   für den ersten Model-Aufruf) oder `seed_turn` mehrfach je Turn aufrufen
//!   — beides verwirft genau die Buchhaltung, die `seed_turn`s eigene Doku
//!   ausdrücklich ausschließt (siehe oben, „ein Aufruf je Model-Runde würde
//!   … bereits verbuchte `context.load`-Aufrufe verwerfen"). Das ist ein
//!   struktureller Umbau der Aufrufreihenfolge dieser Schleife, größer als
//!   dieser Knoten rechtfertigt — dokumentiert als offener Folgeknoten, nicht
//!   halbfertig gebaut.
//!
//! ## Fünfter Nachtrag (dieser Knoten): [`TurnControl`] wird tatsächlich
//! durchgesetzt — vorher las nichts in [`drive_turn`] den Steuerblock
//!
//! Eigene Prüfung: [`TurnControl`] und [`TurnLimits`] existierten bereits vor
//! diesem Knoten (samt vollständiger Prüfpunkt-Logik,
//! `model_checkpoint`/`tool_checkpoint`/`start`/`record_*`), aber **keine**
//! dieser Methoden wurde von [`drive_turn`] je aufgerufen — ein `TurnInput`
//! mit engen [`TurnLimits`] verhielt sich exakt wie eines mit
//! [`TurnLimits::unlimited`]. Dieser Knoten schließt die Lücke:
//!
//! - [`run_turn`]/[`run_turn_durable`] entnehmen `input.control` und reichen
//!   ihn nach [`drive_turn`] durch, das ihn vor jedem Modellaufruf
//!   ([`TurnControl::model_checkpoint`]) und vor jeder Werkzeugausführung
//!   ([`TurnControl::tool_checkpoint`]) befragt. Ein Treffer beendet den Turn
//!   als [`TurnOutcome::Cancelled`] — die Session kehrt über
//!   `AgentSession::complete_turn` nach `Idle` zurück, exakt wie ein
//!   regulärer Abschluss, nur mit `CancelReason` statt stillschweigendem Ende.
//! - `control.start()` markiert den Wanduhr-Nullpunkt einmal je
//!   `drive_turn`-Aufruf; `record_model_round`/`record_usage` laufen nach
//!   jeder Modellantwort, `record_tool_calls` nach bestandenem
//!   `tool_checkpoint`, unmittelbar bevor die Runde tatsächlich dispatcht wird.
//! - **Lücke war offen, ist seit „Siebter Nachtrag" geschlossen:**
//!   [`resume_after_child`] und [`resume_after_approval`] (bzw. ihre
//!   `_durable`-Varianten) sind öffentliche Einstiegspunkte, die von
//!   `harw-tui`, `harw-cli` und den Tests dieses Workspaces mit ihrer
//!   heutigen Signatur aufgerufen werden — ihre Signatur nimmt weiterhin
//!   **keinen** `TurnControl`-Parameter entgegen, das war und bleibt
//!   unverändert. Der damalige Blocker war ein fehlendes session-persistes
//!   Feld außerhalb des Schreibbereichs dieses Knotens (`session.rs`); jener
//!   Schreibbereich schließt seit „Siebter Nachtrag" auch `session.rs` ein.
//!   `AgentSession::active_turn_control` trägt den Block jetzt über die Pause
//!   hinweg, `resume_after_child`/`resume_after_approval` (bzw. ihre
//!   `_durable`-Varianten) lesen ihn darüber zurück, statt bedingungslos einen
//!   frischen `TurnControl::new()` zu bauen — Details siehe „Siebter
//!   Nachtrag" unten.
//!
//! ## Sechster Nachtrag (dieser Knoten, Welle 3): der Modellaufruf racet
//! jetzt tatsächlich, und `ToolsError::Cancelled` bricht mitten in der
//! Ausführung sauber ab
//!
//! Der „Fünfter Nachtrag" oben beschrieb bereits **prüfpunktbasierten**
//! Abbruch (vor dem Modellaufruf, vor der Werkzeugausführung). Zwei Lücken
//! blieben offen, weil beide vorausgesetzte Bausteine erst in einer früheren
//! Welle dieses Knotens entstanden: [`ModelRequest::cancel`] (racebar über
//! [`ModelRequest::with_cancel_token`]) und [`ToolExecutionContext::cancel`]
//! (racebar über `ToolExecutionContext::with_cancel`). Dieser Knoten
//! verdrahtet beide:
//!
//! - **Modellaufruf.** [`drive_turn`] racet `model.respond(request)` per
//!   `tokio::select!` (`biased`) gegen `control.cancel_token().cancelled()`
//!   — der Modellaufruf selbst muss nicht mehr bis zu seiner Antwort
//!   durchlaufen, um einen bereits erfolgten Abbruch zu bemerken. `request`
//!   trägt zusätzlich denselben Token über `with_cancel_token`, damit ein
//!   cancel-fähiger Provider (z. B. `RetryingProvider::race_respond`) auch
//!   innerhalb eigener Retry-Versuche früher abbricht; das `select!` bleibt
//!   der Rückfallpfad für jeden anderen Provider. Meldet `respond()` selbst
//!   [`crate::model::ModelError::Cancelled`] (weil ein cancel-fähiger
//!   Provider den Token vor dem `select!` sah), mündet das in denselben
//!   [`cancel_turn`]-Pfad wie ein Treffer am Prüfpunkt — kein neuer
//!   [`TurnOutcome`], keine neue Fehlervariante.
//! - **Werkzeugausführung.** [`tool_execution_context`] hängt
//!   `control.cancel_token()` an jeden gebauten `ToolExecutionContext` — an
//!   allen drei Aufrufstellen (sequenzieller Dispatch,
//!   [`try_execute_parallel_calls`], [`resume_after_approval_with_store`]).
//!   Meldet ein Ausführer daraufhin `ToolsError::Cancelled`, behandeln beide
//!   Dispatch-Pfade das **nicht** als gewöhnlichen Tool-Fehler (kein
//!   `ToolCallResult::error(...)` ans Modell): der laufende Call bekommt sein
//!   Cancel-Ergebnis, alle noch nicht ausgelieferten Calls derselben Antwort
//!   bekommen ein synthetisches Ergebnis über denselben Mechanismus wie ein
//!   `tool_checkpoint()`-Treffer ([`cancel_turn_with_pending_calls`]
//!   sequenziell; der bereits bestehende `ParallelOutcome::Aborted`-Pfad
//!   parallel — dort stehen alle `tool_call`-Einträge ohnehin schon vor dem
//!   Start der Jobs im Verlauf, sodass nur noch das `tool_result` fehlt).
//!   `resume_after_approval_with_store` bekommt den Token seines eigenen
//!   frischen `TurnControl::new()` (siehe „Fünfter Nachtrag", offene Lücke)
//!   — plumbing, keine neue Dispatch-Reaktion dort.
//!
//! ## Siebter Nachtrag (dieser Knoten): die „Fünfter Nachtrag"-Lücke ist
//! geschlossen — der Steuerblock überlebt Handoff- und Rückfrage-Pausen jetzt
//! tatsächlich, und zwei bisher ungeracete Ausführungspfade racen jetzt auch
//!
//! Der „Fünfter Nachtrag" oben nannte drei offene Punkte: kein session-persister
//! `TurnControl`-Speicher über eine Pause hinweg, ein ungeracetes
//! `executor.traced_execute(...)` im Rückfrage-Fortsetzungspfad, und einen
//! ungeraceten parallelen Join-Loop. Alle drei sind mit diesem Knoten
//! geschlossen:
//!
//! - **Fix A — Steuerblock übersteht die Pause.** [`AgentSession`] trägt jetzt
//!   `active_turn_control: Option<TurnControl>` (`session.rs`, Feld-Doku dort):
//!   [`run_turn`]/[`run_turn_durable`] hinterlegen darauf einen Klon des
//!   `TurnInput::control`-Blocks, sobald der Turn beginnt
//!   (`AgentSession::set_active_turn_control`); [`resume_after_child`]/
//!   [`resume_after_child_durable`] und [`resume_after_approval`]/
//!   [`resume_after_approval_durable`] lesen ihn über
//!   `session.active_turn_control().cloned().unwrap_or_else(TurnControl::new)`
//!   zurück, statt bedingungslos einen frischen, unbegrenzten Block zu bauen.
//!   Der Fallback auf einen frischen Block bleibt für den Fall, dass eine
//!   Session ihren Turn nie über `run_turn`/`run_turn_durable` gestartet hat
//!   (z. B. Tests, die direkt `AgentSession::try_start_turn` rufen) — kein
//!   Panic, keine Regression für Aufrufer außerhalb dieses Knotens.
//!   `AgentSession::complete_turn` löscht das Feld wieder, symmetrisch zu
//!   `current_turn`. Ein `CancelToken`, der vor einer Handoff- oder
//!   Rückfrage-Pause abgebrochen wurde (Ctrl+C), erreicht damit jetzt auch die
//!   Fortsetzung nach der Pause — vorher verschluckte der frische Token genau
//!   diesen Fall stillschweigend.
//! - **Fix B — der Rückfrage-Fortsetzungspfad racet jetzt auch.**
//!   `resume_after_approval_with_store`s Zweig für einen genehmigten,
//!   nicht-Handoff-Call (der „Rückfrage"-Pfad, nach einem `AskUser`) hatte
//!   keine `was_cancelled`-Weiche — ein hier hängender Ausführer war gegen
//!   Cancel vollständig blind, unabhängig davon, ob er selbst kooperativ ist.
//!   Der Aufruf racet jetzt exakt wie im sequenziellen Pfad (`tokio::select!`,
//!   `biased`, `control.cancel_token().cancelled()` gegen
//!   `executor.traced_execute(...)`) und liefert bei einem Treffer ein
//!   synthetisches `Err(ToolsError::Cancelled)`; ein Treffer geht über
//!   [`cancel_turn_with_pending_calls`] mit `remaining: Vec::new()` — ein
//!   Rückfrage-Resume setzt genau einen Call fort, die übrigen Calls derselben
//!   Modellantwort wurden beim `AskUser`-Treffer nie in den Verlauf
//!   geschrieben (siehe `ApprovalDecision::AskUser` im sequenziellen Pfad) und
//!   sind daher keine offenen Geschwister, die noch ein Ergebnis bräuchten.
//!   Der sequenzielle Dispatch-Pfad selbst bekam denselben Race schon
//!   zusätzlich um den ungeraceten `.await` seines Werkzeugaufrufs herum —
//!   vorher konnte ein Ausführer, der `ctx.cancel()` nie selbst abfragt, den
//!   Turn bis zu seinem eigenen Ende blockieren; jetzt beendet ein Treffer den
//!   `select!` synchron mit einem synthetischen `Err(ToolsError::Cancelled)`.
//! - **Fix C — der parallele Join-Loop racet jetzt auch.**
//!   [`try_execute_parallel_calls`] wartete mit
//!   `while let Some(joined) = joins.join_next().await` ungeracet auf **alle**
//!   Aufrufe der `JoinSet`, bevor überhaupt auf Cancel reagiert wurde — ein
//!   nicht kooperativer Ausführer blockierte damit die gesamte Antwort. Die
//!   Reaper-Schleife racet jetzt selbst gegen `control.cancel_token()`: ein
//!   Treffer setzt `cancel_hit` und ruft `joins.abort_all()`; danach werden
//!   weiterhin abgeschlossene Jobs abgeholt (`join_next()` bleibt aktiv), ein
//!   `JoinError` eines abgebrochenen Jobs wird nach einem Cancel-Treffer
//!   verworfen statt propagiert (vorher — ohne Cancel-Treffer — bleibt ein
//!   `JoinError` weiterhin ein echter Fehler dieser Funktion). Jeder Slot, der
//!   danach noch `None` ist (abgebrochen, bevor sein Ergebnis geerntet wurde),
//!   bekommt synthetisch dasselbe `"turn cancelled before delivery"`-Ergebnis,
//!   das die bereits bestehende `TurnGuard`-Abort-Auslieferung darunter auch
//!   für andere Abbruchgründe erzeugt — die Auslieferungsschleife selbst
//!   bleibt unverändert, sie verlangt nur, dass jeder Slot `Some(...)` ist,
//!   was jetzt garantiert ist.
//!
//! ## Achter Nachtrag (dieser Knoten): hängender `Running`-Zustand nach einem
//! Fehler beim Rückfrage-Resume, und ein Fortschritts-Hinweis, den ein Modell
//! als Antwort-Cache fehldeutete
//!
//! - **Fix A — jeder Fehlerausstieg von [`resume_after_approval_with_store`]
//!   nach erfolgreichem `resolve_approval` durchläuft jetzt
//!   [`transition_after_turn_failure`].** Vorher gaben mehrere Stellen (jedes
//!   `?` nach dem Rückfrage-Resume, sowie zwei `return drive_turn(...)`/
//!   `return cancel_turn_with_pending_calls(...)`) ihr `Result` direkt aus der
//!   Funktion zurück — anders als [`run_turn_with_approvals`] und
//!   [`resume_after_child_with_approvals`], die diesen Übergang bereits
//!   korrekt anwenden. Ein Transport-Fehler des Modells während eines
//!   fortgesetzten Rückfrage-Turns ließ die Session dadurch dauerhaft in
//!   `Running` hängen; der nächste Turn scheiterte mit „session not idle:
//!   Running", obwohl der ursprüngliche Fehler retrybar war. Der gesamte
//!   Auflösungspfad (der `match resolution { ... }`-Ausdruck) läuft jetzt in
//!   einem inneren `async`-Block; `return`/`?` darin beenden nur diesen Block,
//!   nicht die Funktion — genau ein `Result` wird danach geprüft und im
//!   Fehlerfall genau einmal an [`transition_after_turn_failure`] gereicht,
//!   bevor es zurückgegeben wird.
//! - **Fix C — Polling zählt als Fortschritt.** [`apply_tool_guard`] wertete
//!   einen erfolgreichen Tool-Aufruf nur dann als Rundenfortschritt, wenn
//!   seine Signatur (Name + kanonische Argumente) in diesem Turn noch nicht
//!   gesehen wurde. Für [`crate::guard::POLLING_TOOLS`] (aktuell nur
//!   `shell.exec`) ist ein wiederholter identischer Aufruf jedoch legitimes
//!   Warten auf einen laufenden Hintergrundprozess, kein Stillstand — ein
//!   Modell, das so pollte, sah stattdessen den
//!   [`crate::guard::TurnGuard::observe_round_end`]-Hinweis zu ausbleibendem
//!   Fortschritt und deutete ihn als Beleg für einen nicht existenten
//!   Antwort-Cache, woraufhin es Befehle künstlich variierte, um „neue"
//!   Aufrufe zu erzwingen. Ein wiederholter *erfolgreicher* Aufruf eines
//!   `POLLING_TOOLS`-Eintrags zählt deshalb jetzt unabhängig von seiner
//!   Signatur als Fortschritt, und der Hinweistext benennt das Missverständnis
//!   jetzt explizit („Es gibt keinen Antwort-Cache — jeder Aufruf wurde echt
//!   ausgeführt"). Die Schwellenwerte selbst (`no_progress_rounds_warn`/
//!   `_abort`) sind unverändert.
//!
use crate::cancel::{CancelReason, CancelToken};
use crate::capture::{ToolOutcome, ToolOutcomeStatus};
use crate::error::{CoreError, CoreResult};
use crate::guard::{DriftEvent, DriftKind, GuardVerdict, TurnGuard};
use crate::model::{ModelProvider, ModelRequest, RequestIdentity};
use crate::session::{AgentSession, SpawnContext, TurnHandle};
use crate::state_store::{StateStore, StateStoreError, UsageRound};
use harw_extension_api::{
    ApprovalDecision, LoadedInstructions, SpawnInput, ToolExecutor, TurnInputContext,
    TurnStartInput, TurnStopInput,
};
use harw_protocol::events::TurnEvent;
use harw_protocol::items::{
    AssistantMessageItem, ContentPart, OpaqueReasoning, ReasoningItem, ToolCallResult, TurnItem,
};
use harw_session_store::{ApprovalRecord, ApprovalStore};
use harw_tools::{
    ToolCall, ToolExecutionContext, ToolName, ToolOutput, ToolSpec, ToolsError, TracedToolExecutor,
};
use harw_types::{
    ApprovalActor, Clock, ReviewDecision, SessionId, SystemClock, TokenUsage, ToolCallId,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use tracing::Instrument as _;

/// Präfix, an dem ein Tool-Call als Handoff erkannt wird (Agents-SDK-Muster
/// `transfer_to_<role>`).
pub const HANDOFF_PREFIX: &str = "transfer_to_";

/// Ergebnis-Slot für einen parallelen Tool-Call: (ID, Ergebnis, Wandzeit ms).
// Letztes Feld: `true`, wenn dieser Call mit `ToolsError::Cancelled`
// endete (siehe `try_execute_parallel_calls`s Ergebnis-Auslieferung).
type ParallelCallSlot = Option<(
    ToolCallId,
    ToolCallResult,
    u64,
    String,
    serde_json::Value,
    bool,
)>;

/// Grenzwerte eines einzelnen Turns (W4a A-LOOP).
///
/// # Description
/// Feldgleiche Kernfassung von `harw_runtime::TurnLimits`
/// (`harw-runtime/src/assembly.rs`): `harw-runtime` hängt an `harw-core`, der
/// Turn-Loop kann den Laufzeittyp deshalb nicht importieren. Die Laufzeit
/// überträgt ihre abgeleiteten Werte feldweise (Folgearbeit im Ledger
/// `W4a/A-LOOP.md`). Durchgesetzt wird in [`run_turn`] und den
/// `resume_*`-Einstiegen: vor jedem Modellaufruf und vor jeder
/// Werkzeugausführung.
///
/// # Concurrency
/// `Copy`-Datenhalter ohne innere Veränderlichkeit.
///
/// # Examples
/// ```rust
/// use harw_core::turn_loop::TurnLimits;
///
/// let limits = TurnLimits { max_model_rounds: 4, ..TurnLimits::unlimited() };
/// assert_eq!(limits.max_tool_calls, u32::MAX);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnLimits {
    /// Maximale Anzahl Modell-Runden eines Turns.
    pub max_model_rounds: u32,
    /// Maximale Anzahl Werkzeugaufrufe (inklusive Handoffs) eines Turns.
    pub max_tool_calls: u32,
    /// Maximale Gesamtzahl erzeugter Ausgabe-Tokens.
    pub max_output_tokens_total: u64,
    /// Maximale Wanduhrzeit eines Turns (monoton gemessen, ab Turn-Start).
    pub wall_time: Duration,
    /// Obergrenze eines einzelnen gerenderten Werkzeugergebnisses in Bytes;
    /// `usize::MAX` heißt „keine Kappung" und wird dem Provider nicht gemeldet.
    pub tool_result_max_bytes: usize,
}

impl TurnLimits {
    /// Returns limits that never trip (every field at its type maximum).
    ///
    /// # Returns
    /// A [`TurnLimits`] equal to [`TurnLimits::default`].
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            max_model_rounds: u32::MAX,
            max_tool_calls: u32::MAX,
            max_output_tokens_total: u64::MAX,
            wall_time: Duration::MAX,
            tool_result_max_bytes: usize::MAX,
        }
    }

    // Provider-Hinweis für `ModelRequest::tool_result_max_bytes`; unbegrenzt ⇒ `None`.
    fn tool_result_max_bytes_hint(&self) -> Option<usize> {
        (self.tool_result_max_bytes != usize::MAX).then_some(self.tool_result_max_bytes)
    }
}

impl Default for TurnLimits {
    fn default() -> Self {
        Self::unlimited()
    }
}

// Zähler eines Turns, geteilt zwischen allen Klonen eines `TurnControl`.
#[derive(Debug, Default)]
struct TurnMeter {
    started: Option<Instant>,
    model_rounds: u32,
    tool_calls: u32,
    usage: TokenUsage,
}

/// Steuerblock eines Turns: Abbruch, Grenzwerte, Serveruhr und Zähler.
///
/// # Description
/// Ein `TurnControl` gehört zu **genau einem** Turn. Klone teilen denselben
/// [`CancelToken`]-Knoten und denselben Zähler; wer einen pausierten Turn mit
/// `resume_*` fortsetzt, übergibt deshalb einen Klon des Blocks, mit dem der
/// Turn gestartet wurde — nur dann zählen Runden, Aufrufe, Nutzung und
/// Wanduhr über die Pause hinweg weiter. Ein frischer Block setzt die Zähler
/// zurück.
///
/// Die Uhr liefert Zeitstempel für dauerhafte Freigaben (`issued_at`,
/// `ApprovalStore::resolve`) und Kind-Abschlüsse; die Wanduhrgrenze misst
/// dagegen monoton über [`Instant`].
///
/// # Concurrency
/// `Send + Sync`; der Zähler liegt hinter einem `Mutex`, der nie über einen
/// `.await` gehalten wird.
///
/// # Examples
/// ```rust
/// use harw_core::cancel::{CancelReason, CancelToken};
/// use harw_core::turn_loop::{TurnControl, TurnInput, TurnLimits};
///
/// let token = CancelToken::new();
/// let control = TurnControl::new()
///     .with_cancel(token.clone())
///     .with_limits(TurnLimits { max_model_rounds: 8, ..TurnLimits::unlimited() });
/// let input = TurnInput::user("hallo").with_control(control.clone());
/// token.cancel(CancelReason::User);
/// assert_eq!(input.control.cancel_token().reason(), Some(CancelReason::User));
/// ```
#[derive(Clone)]
pub struct TurnControl {
    cancel: CancelToken,
    limits: TurnLimits,
    clock: Arc<dyn Clock>,
    meter: Arc<Mutex<TurnMeter>>,
}

impl std::fmt::Debug for TurnControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnControl")
            .field("cancel", &self.cancel)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl Default for TurnControl {
    fn default() -> Self {
        Self::new()
    }
}

impl TurnControl {
    /// Creates a control block with a fresh token, unlimited limits and the system clock.
    ///
    /// # Returns
    /// A new [`TurnControl`] whose counters are all zero.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cancel: CancelToken::new(),
            limits: TurnLimits::unlimited(),
            clock: Arc::new(SystemClock),
            meter: Arc::new(Mutex::new(TurnMeter::default())),
        }
    }

    /// Replaces the cancellation token (e.g. a child of the caller's token).
    ///
    /// # Arguments
    /// - `cancel` (`CancelToken`): token observed at every checkpoint.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Replaces the turn limits.
    ///
    /// # Arguments
    /// - `limits` (`TurnLimits`): enforced before model calls and tool executions.
    #[must_use]
    pub fn with_limits(mut self, limits: TurnLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Replaces the server clock used for approval and child timestamps.
    ///
    /// # Arguments
    /// - `clock` (`Arc<dyn Clock>`): shared clock; tests pass a fixed clock.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Returns the cancellation token observed by this turn.
    #[must_use]
    pub fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }

    /// Returns the enforced limits.
    #[must_use]
    pub fn limits(&self) -> &TurnLimits {
        &self.limits
    }

    /// Returns the server clock.
    #[must_use]
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// Returns the token usage accumulated over all model rounds so far.
    ///
    /// # Concurrency
    /// Briefly locks the shared counter.
    #[must_use]
    pub fn usage(&self) -> TokenUsage {
        self.meter().usage.clone()
    }

    /// Returns the number of model rounds started so far.
    #[must_use]
    pub fn model_rounds(&self) -> u32 {
        self.meter().model_rounds
    }

    /// Returns the number of tool calls (including handoffs) dispatched so far.
    #[must_use]
    pub fn tool_calls(&self) -> u32 {
        self.meter().tool_calls
    }

    // Ein vergifteter Mutex hält nur Zähler: der letzte Stand bleibt gültig.
    fn meter(&self) -> MutexGuard<'_, TurnMeter> {
        self.meter.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // Startet die Wanduhr beim ersten Aufruf; weitere Aufrufe ändern nichts.
    fn start(&self) {
        let mut meter = self.meter();
        if meter.started.is_none() {
            meter.started = Some(Instant::now());
        }
    }

    // Addiert die Nutzung einer Modellrunde.
    fn record_usage(&self, usage: &TokenUsage) {
        self.meter().usage.add(usage);
    }

    // Zählt eine begonnene Modellrunde.
    fn record_model_round(&self) {
        let mut meter = self.meter();
        meter.model_rounds = meter.model_rounds.saturating_add(1);
    }

    // Zählt `count` ausgelöste Werkzeugaufrufe.
    fn record_tool_calls(&self, count: usize) {
        let mut meter = self.meter();
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        meter.tool_calls = meter.tool_calls.saturating_add(count);
    }

    // `true`, wenn die Wanduhrgrenze erreicht ist.
    fn wall_time_exceeded(&self, meter: &TurnMeter) -> bool {
        meter
            .started
            .is_some_and(|started| started.elapsed() >= self.limits.wall_time)
    }

    // Prüfpunkt vor einem Modellaufruf: Abbruch, Runden, Ausgabe-Tokens, Wanduhr.
    fn model_checkpoint(&self) -> Option<CancelReason> {
        if let Some(reason) = self.cancel.reason() {
            return Some(reason);
        }
        let meter = self.meter();
        let exhausted = meter.model_rounds >= self.limits.max_model_rounds
            || meter.usage.output_tokens >= self.limits.max_output_tokens_total
            || self.wall_time_exceeded(&meter);
        exhausted.then_some(CancelReason::Budget)
    }

    // Prüfpunkt vor dem Auslösen von `count` Werkzeugaufrufen.
    fn tool_checkpoint(&self, count: usize) -> Option<CancelReason> {
        if let Some(reason) = self.cancel.reason() {
            return Some(reason);
        }
        let meter = self.meter();
        let requested =
            u64::from(meter.tool_calls).saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        let exhausted =
            requested > u64::from(self.limits.max_tool_calls) || self.wall_time_exceeded(&meter);
        exhausted.then_some(CancelReason::Budget)
    }
}

/// Eingabe, mit der ein Turn gestartet wird.
#[derive(Debug, Clone, Default)]
pub struct TurnInput {
    /// Optionaler User-Text, der zu Beginn in den Verlauf gespielt wird.
    pub user_text: Option<String>,
    /// Metadaten, die dem `TurnInputContext` mitgegeben werden.
    pub metadata: serde_json::Value,
    /// Steuerblock (Abbruch, Grenzwerte, Uhr, Zähler) dieses Turns.
    pub control: TurnControl,
}

impl TurnInput {
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            user_text: Some(text.into()),
            metadata: serde_json::Value::Null,
            control: TurnControl::new(),
        }
    }

    /// Replaces the control block of this input.
    ///
    /// # Arguments
    /// - `control` (`TurnControl`): keep a clone to pass to `resume_*` later.
    #[must_use]
    pub fn with_control(mut self, control: TurnControl) -> Self {
        self.control = control;
        self
    }
}

/// Wie ein Turn endete.
#[derive(Debug)]
pub enum TurnOutcome {
    /// Turn vollständig abgeschlossen (keine Tool-Calls mehr).
    Completed,
    /// Ein Handoff-Tool wurde gerufen — die Session wartet jetzt auf das Child.
    /// Wiederaufnahme über `resume_after_child`.
    AwaitingChild {
        child: SessionId,
        call_id: ToolCallId,
        role: String,
    },
    /// Ein Guardrail verlangt eine Nutzer-Entscheidung (`AskUser`). Der Turn
    /// pausiert, bis die Entscheidung über den Channel zurückkommt.
    AwaitingApproval {
        call_id: ToolCallId,
        request: harw_types::ItemId,
    },
    /// Der Turn wurde an einem Prüfpunkt abgebrochen: über den
    /// [`CancelToken`] (`reason` = dessen Grund) oder weil eine
    /// [`TurnLimits`]-Grenze erreicht war (`CancelReason::Budget`). Offene
    /// Tool-Calls haben ein synthetisches Fehlerergebnis; die Session ist `Idle`.
    Cancelled { reason: CancelReason },
    /// Die Modellausgabe wurde abgeschnitten (`StopReason::MaxTokens` oder
    /// `ContextWindowExceeded`). Tool-Calls der Antwort wurden nicht ausgeführt;
    /// die Session ist `Idle`.
    Truncated,
    /// Das Modell hat abgelehnt (`StopReason::Refusal` oder `ContentFilter`).
    /// Tool-Calls der Antwort wurden nicht ausgeführt; die Session ist `Idle`.
    Refused { detail: Option<String> },
    /// Eine angenommene Wiederaufnahme konnte nicht fortgesetzt werden (bzw.
    /// die dauerhafte Freigabe ist abgelaufen/defekt). Die Session ist `Failed`.
    Failed { reason: String },
}

/// Resolution supplied by the user for a paused approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalResolution {
    Approve,
    Reject { reason: String },
}

impl From<ReviewDecision> for ApprovalResolution {
    fn from(value: ReviewDecision) -> Self {
        match value {
            ReviewDecision::Approved | ReviewDecision::ApprovedOnce => Self::Approve,
            ReviewDecision::Rejected => Self::Reject {
                reason: "rejected by user".to_owned(),
            },
        }
    }
}

impl ApprovalResolution {
    /// Die eine Ablehnung, die jede Oberfläche liefert, sobald
    /// [`crate::session::PendingApproval::timeout_at`] erreicht ist, ohne
    /// dass eine ausdrückliche Entscheidung eintraf (Interaktionsvertrag
    /// §4.4). Eine gemeinsame Konstruktionsstelle statt einer Kopie der
    /// Begründung je Front-End.
    ///
    /// # Returns
    /// [`Self::Reject`] mit [`crate::session::APPROVAL_TIMEOUT_REASON`].
    #[must_use]
    pub fn timed_out() -> Self {
        Self::Reject {
            reason: crate::session::APPROVAL_TIMEOUT_REASON.to_owned(),
        }
    }
}

/// Sammelt Kontext von allen `ContextProvider`n und filtert nach der
/// session-level [`SessionActivation`][crate::activation::SessionActivation].
///
/// # Description
/// Iterates every registered `ContextProvider`, but calls
/// [`harw_extension_api::ContextProvider::contribute_v2`] instead of the
/// legacy `contribute` — **this is the production caller referenced in this
/// module's "Zweiter Nachtrag"/"Dritter Nachtrag" sections**: every provider
/// that does not override `contribute_v2` still runs through it via its
/// default bridge (`fragment_from_v1`), so `contribute_v2` is now reached on
/// every turn, not only from `harw-extension-api`'s own tests.
///
/// **Fünfter Nachtrag (dieser Knoten):** vor diesem Knoten projizierte diese
/// Funktion jedes zurückgegebene [`harw_context::Fragment`] sofort zurück auf
/// das alte [`ContextFragment`] (`label` + `content` ← `label.as_str()` +
/// `body`), weil `ModelRequest::with_context_budget`
/// (`harw-core/src/model.rs`) ausschließlich `ContextFragment` annahm. Dieser
/// Knoten hat Schreibzugriff auf `model.rs` bekommen und dort
/// `ModelRequest::with_context_program` ergänzt, das native
/// `harw_context::Fragment`s entgegennimmt (siehe dessen Moduldoku, Abschnitt
/// „Zwei Wege zur Kontextmontage") — die Rückwandlung ist deshalb **nicht
/// mehr hier** nötig; sie passiert (wenn überhaupt) jetzt in `model.rs`s
/// Rückfallpfad, für eine Sitzung ohne Programm oder ohne Decke. Diese
/// Funktion liefert seit diesem Knoten die von `contribute_v2` gelieferten
/// `harw_context::Fragment`s **unverändert** (nach dem Aktivierungsfilter)
/// zurück — keine Information geht mehr auf dem Weg zu `drive_turn` verloren.
///
/// Fragmente werden weiterhin exakt wie vor diesem Knoten gefiltert: ein
/// Fragment, dessen Label in der Aktivierung der Sitzung deaktiviert ist,
/// wird verworfen. Ein v1-Fragment mit **leerem** Label wird von
/// `contribute_v2`s Vorgabe-Bridge bereits vorher verworfen (bei der
/// `FragmentLabel::try_new`-Prüfung) — siehe
/// `harw_extension_api::fragment_from_v1`s eigene Dokumentation; kein
/// Provider oder Test in diesem Workspace konstruiert ein leeres Label.
///
/// # Arguments
/// - `session` (`&AgentSession`): session providing both the registry and the
///   activation filter.
/// - `ctx` (`&TurnInputContext`): turn metadata forwarded to each provider.
///
/// # Returns
/// Filtered list of [`harw_context::Fragment`]s that are model-visible, in
/// arrival order (order has no effect on the deterministic montage in
/// `crate::context_budget::Assembly`).
///
/// # Concurrency
/// `async`; no locks held across `.await` points.
pub async fn gather_context(
    session: &AgentSession,
    ctx: &TurnInputContext,
) -> Vec<harw_context::Fragment> {
    let activation = session.activation();
    let mut fragments = Vec::new();
    for provider in session.registry().context_providers() {
        let contributed = provider.contribute_v2(ctx).await;
        for fragment in contributed {
            let label_str = fragment.label.as_str();
            let filter_label = if label_str.is_empty() {
                "unlabeled"
            } else {
                label_str
            };
            if activation.is_context_enabled(filter_label) {
                fragments.push(fragment);
            }
        }
    }
    fragments
}

/// Baut den Delegationsziel-Kontextblock (Nachtrag F) aus bereits sortierten
/// Rollennamen — oder `None`, wenn keine Ziele sichtbar sind.
///
/// # Arguments
/// - `names` (`&[String]`): exakte, sortierte Rollennamen aus
///   [`harw_extension_api::AgentSpawner::delegation_target_names`].
///
/// # Returns
/// `None` bei leerer Liste oder wenn die (statischen, stets gültigen)
/// Label-/Sektionsnamen unerwartet nicht konstruierbar wären — dann bleibt
/// der Turn ohne diesen Block, statt zu scheitern.
fn delegation_targets_fragment(names: &[String]) -> Option<harw_context::Fragment> {
    if names.is_empty() {
        return None;
    }
    let body = format!(
        "Delegierbare Ziele (transfer_to_<name>): {}",
        names.join(", ")
    );
    let label = harw_context::FragmentLabel::try_new("delegation.targets").ok()?;
    let section = harw_context::SectionName::try_new("delegation.targets").ok()?;
    Some(harw_context::Fragment {
        label,
        section,
        trust: harw_context::TrustClass::Instruction,
        stability: harw_context::Stability::Stable,
        origin: harw_context::FragmentOrigin {
            provider: "turn_loop.delegation_targets".to_owned(),
            namespace: "core".to_owned(),
            produced_at: jiff::Timestamp::now(),
        },
        cost: harw_lens_types::CostEstimate(body.len() as u32),
        digest: harw_types::ContentDigest::of(body.as_bytes()),
        body,
    })
}

/// Wickelt eine Fortsetzungs-Instruktion nach einem `MaxTokens`-Abbruch in
/// einen [`harw_context::Fragment`]-Block (Trust-Klasse `Instruction`), damit
/// sie wie die übrigen Instructions in die Kontextmontage eingeht. Ein
/// Fehlschlag der Newtype-Validierung ist kein Turn-Fehler: ohne den Hinweis
/// liefert der Provider eben eine zweiteilige statt einer nahtlosen Antwort.
fn continuation_fragment(body: &str) -> Option<harw_context::Fragment> {
    let label = harw_context::FragmentLabel::try_new("continuation.instruction").ok()?;
    let section = harw_context::SectionName::try_new("continuation.instruction").ok()?;
    Some(harw_context::Fragment {
        label,
        section,
        trust: harw_context::TrustClass::Instruction,
        stability: harw_context::Stability::Stable,
        origin: harw_context::FragmentOrigin {
            provider: "turn_loop.continuation".to_owned(),
            namespace: "core".to_owned(),
            produced_at: jiff::Timestamp::now(),
        },
        cost: harw_lens_types::CostEstimate(body.len() as u32),
        digest: harw_types::ContentDigest::of(body.as_bytes()),
        body: body.to_owned(),
    })
}

/// Entscheidet, ob die laufende Session die UIA-Root-Session ist — das
/// einzige organisatorische Kriterium, unter dem Reasoning-Text sichtbar
/// gemacht wird (Welle 3 — 3e).
///
/// # Beschreibung
/// Identisches Kriterium zu `is_uia_root_session` in
/// `harw-tui/src/session_controller.rs:473-480`: kein Parent UND
/// Organisationsrolle `UserInterface`. Diese Datei kann jene Funktion nicht
/// wiederverwenden (anderer Crate, keine gemeinsame Abhängigkeit), daher
/// dieselbe Prüfung hier dupliziert statt einer neuen crate-übergreifenden
/// Kopplung. Kind-/Worker-Sessions (jede mit Parent) und Root-Sessions
/// anderer Rollen (z. B. `RootOrchestrator`) liefern `false`.
///
/// # Arguments
/// - `session` (`&AgentSession`): die laufende Session.
///
/// # Returns
/// `true` genau dann, wenn `session.parent_session_id()` `None` ist und
/// `session.spawn_context().organizational_role == AgentRoleId::UserInterface`.
fn is_uia_root_session(session: &AgentSession) -> bool {
    session.parent_session_id().is_none()
        && session
            .spawn_context()
            .map(|context| context.organizational_role)
            == Some(harw_agent_dsl::roles::AgentRoleId::UserInterface)
}

// Baut die per-Request-Identität für optionale Gateway-Header (`x-harw-*`),
// die `drive_turn` unten an `ModelRequest::with_identity` übergibt.
//
// `agent` ist die eigene Session-ID. `session` soll die Wurzel-Session des
// Agentenbaums gruppieren — `SpawnContext` (siehe `harw-core/src/session.rs`)
// trägt dafür kein eigenes Feld, nur `organizational_role`, deshalb ist der
// direkte Parent (`session.parent_session_id()`) die nächstbeste verfügbare
// Näherung: ein Kind kennt nur seinen unmittelbaren Elternteil, keine
// mehrstufige Kette bis zur Wurzel. Ohne Parent ist die Session selbst die
// Wurzel. `role` liest `SpawnContext::organizational_role`; fehlt der
// `SpawnContext` ganz (kein tatsächlich modellierter Root — jeder modellierte
// Root, siehe `is_uia_root_session` oben, trägt einen `SpawnContext`, z. B.
// eine bare Test-Session), fällt `role` auf `"main"` zurück.
fn request_identity(session: &AgentSession) -> RequestIdentity {
    let agent = session.id().as_str().to_owned();
    let session_id = session
        .parent_session_id()
        .map(|parent| parent.as_str().to_owned())
        .unwrap_or_else(|| agent.clone());
    let role = session
        .spawn_context()
        .map(|context| organizational_role_str(context.organizational_role))
        .unwrap_or("main")
        .to_owned();
    RequestIdentity {
        session: session_id,
        agent,
        role,
    }
}

// Kebab-case-Darstellung von `AgentRoleId`, identisch zu dessen
// `#[serde(rename_all = "kebab-case")]` (`harw-agent-dsl/src/roles.rs`) — das
// Enum trägt selbst kein `as_str`/`Display`.
fn organizational_role_str(role: harw_agent_dsl::roles::AgentRoleId) -> &'static str {
    use harw_agent_dsl::roles::AgentRoleId;
    match role {
        AgentRoleId::UserInterface => "user-interface",
        AgentRoleId::RootOrchestrator => "root-orchestrator",
        AgentRoleId::ChildOrchestrator => "child-orchestrator",
        AgentRoleId::Worker => "worker",
        AgentRoleId::UiaWorker => "uia-worker",
        AgentRoleId::AgentSteward => "agent-steward",
    }
}

/// Extrahiert lesbaren Denktext aus den Blöcken eines `OpaqueReasoning`
/// (Welle 3 — 3e; Block-Typen `"reasoning"`/`"reasoning_content"` ergänzt in
/// Teil 2 / Datei B des Reasoning-Sichtbarkeits-Plans).
///
/// # Beschreibung
/// Unterstützt drei Block-Formen, je nach Provider/Transport:
/// - `"thinking"` (Anthropic Messages API): Text direkt aus dem
///   `"thinking"`-Feld.
/// - `"reasoning"` (OpenAI-Responses-Transport): Text aus allen Einträgen
///   des `"summary"`-Arrays vom Typ `"summary_text"`, deren `"text"`-Felder
///   mit `\n` verbunden werden (dasselbe Verkettungsmuster wie für mehrere
///   `"thinking"`-Blöcke unten). Ein leeres oder fehlendes `"summary"`-Array
///   sowie ausschließlich leere `"text"`-Felder liefern für diesen Block
///   keinen Text (zählt nicht als „gefunden").
/// - `"reasoning_content"` (Chat-Transport, DeepSeek/Kimi/GLM-Konvention):
///   Text direkt aus dem `"text"`-Feld, wenn nicht leer.
///
/// `"redacted_thinking"` (Anthropic, verschlüsselt) und jeder andere/
/// unbekannte Block-Typ liefern keinen extrahierbaren Text und werden
/// stillschweigend übersprungen — robust gegen fehlende Felder, kein Panic,
/// kein Fehler. Eine gemischte Blockliste (in der Praxis nie vom selben
/// Provider gemeinsam geliefert) wird pro Block einzeln ausgewertet und die
/// gefundenen Texte werden wie bei mehreren `"thinking"`-Blöcken mit `\n`
/// verbunden.
///
/// Bewusst lokal in `turn_loop.rs` statt in `harw-provider-http` verortet:
/// letzteres Crate wird parallel von einem anderen Agenten bearbeitet; die
/// Extraktion gehört logisch dorthin (siehe
/// `harw_provider_http::anthropic::extract_anthropic_reasoning`), wird hier
/// aber bewusst dupliziert, um keine Datei außerhalb dieser zu berühren.
///
/// # Arguments
/// - `reasoning` (`&OpaqueReasoning`): die vom Provider gelieferten,
///   unverändert erhaltenen Denkblöcke.
///
/// # Returns
/// `Some(String)` — die pro Block extrahierten Texte in Blockreihenfolge,
/// mit `\n` verbunden — wenn mindestens ein Block extrahierbaren Text trug;
/// sonst `None` (z. B. nur `redacted_thinking`-/unbekannte Blöcke, leere
/// `summary`-Arrays oder eine leere Blockliste).
fn extract_thinking_text(reasoning: &OpaqueReasoning) -> Option<String> {
    /// Extrahiert den lesbaren Text eines einzelnen `"reasoning"`-Blocks
    /// (OpenAI Responses) aus dessen `"summary"`-Array: alle Einträge vom
    /// Typ `"summary_text"` mit nicht-leerem `"text"`-Feld, mit `\n`
    /// verbunden. Lokale Hilfsfunktion, kein eigener Doc-Header nötig.
    fn extract_reasoning_summary_text(block: &serde_json::Value) -> Option<String> {
        let summary = block.get("summary").and_then(serde_json::Value::as_array)?;
        let mut joined = String::new();
        for entry in summary {
            if entry.get("type").and_then(serde_json::Value::as_str) != Some("summary_text") {
                continue;
            }
            let Some(text) = entry.get("text").and_then(serde_json::Value::as_str) else {
                continue;
            };
            if text.is_empty() {
                continue;
            }
            if !joined.is_empty() {
                joined.push('\n');
            }
            joined.push_str(text);
        }
        (!joined.is_empty()).then_some(joined)
    }

    let mut joined = String::new();
    for block in &reasoning.blocks {
        let block_text = match block.get("type").and_then(serde_json::Value::as_str) {
            Some("thinking") => block
                .get("thinking")
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned),
            Some("reasoning") => extract_reasoning_summary_text(block),
            Some("reasoning_content") => block
                .get("text")
                .and_then(serde_json::Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned),
            _ => None,
        };
        let Some(block_text) = block_text else {
            continue;
        };
        if !joined.is_empty() {
            joined.push('\n');
        }
        joined.push_str(&block_text);
    }
    (!joined.is_empty()).then_some(joined)
}

/// Lädt Instructions von allen `InstructionsProvider` und filtert nach der
/// session-level [`SessionActivation`][crate::activation::SessionActivation].
///
/// # Description
/// Iterates every registered `InstructionsProvider` and merges their output
/// into a single [`LoadedInstructions`]. The session activation is applied
/// using label conventions:
/// - The first non-empty `system_prompt` across all providers uses the label
///   `"baseline"`. Subsequent non-empty system prompts (if any provider
///   returned one and `combined.system_prompt` is already set) would use
///   `"extra_<index>"`, but the underlying merge already ignores them.
/// - Extra `fragments` from each provider use the label
///   `"fragment_<provider_index>_<fragment_index>"`.
///
/// If a label is disabled via [`SessionActivation::disable_instructions`], the
/// corresponding content is dropped.
///
/// # Arguments
/// - `session` (`&AgentSession`): session providing both the registry and the
///   activation filter.
///
/// # Returns
/// Merged, filtered [`LoadedInstructions`].
///
/// # Concurrency
/// `async`; no locks held across `.await` points.
pub async fn load_instructions(session: &AgentSession) -> LoadedInstructions {
    let activation = session.activation();
    let mut combined = LoadedInstructions::default();
    for (provider_idx, provider) in session
        .registry()
        .instructions_providers()
        .iter()
        .enumerate()
    {
        let loaded = provider.load().await;
        // System prompt: first non-empty one wins with label "baseline";
        // subsequent providers' system_prompts are implicitly labelled
        // "extra_<n>" and skipped if already combined (existing logic).
        if !loaded.system_prompt.is_empty()
            && combined.system_prompt.is_empty()
            && activation.is_instructions_enabled("baseline")
        {
            combined.system_prompt = loaded.system_prompt;
        }
        // Fragments: each gets a deterministic label for filtering.
        for (frag_idx, fragment) in loaded.fragments.into_iter().enumerate() {
            let label = format!("fragment_{provider_idx}_{frag_idx}");
            if activation.is_instructions_enabled(&label) {
                combined.fragments.push(fragment);
            }
        }
    }
    combined
}

/// Benachrichtigt alle Turn-Observer über Start.
pub fn notify_turn_start(session: &AgentSession, input: &TurnStartInput) {
    for observer in session.registry().turn_observers() {
        observer.on_turn_start(input);
    }
}

/// Benachrichtigt alle Turn-Observer über Stop.
pub fn notify_turn_stop(session: &AgentSession, input: &TurnStopInput) {
    for observer in session.registry().turn_observers() {
        observer.on_turn_stop(input);
    }
}

/// Sendet ein [`TurnEvent`] an den optionalen Live-Event-Sink der Session.
/// No-op, wenn kein Sink konfiguriert ist oder der Empfänger bereits
/// abgehängt hat (Best-Effort, blockiert den Turn-Hot-Path nie).
fn emit(session: &AgentSession, event: TurnEvent) {
    session.live_emitter().emit(event);
}

/// Baut den Streaming-Sink einer Modell-Runde: Text-/Reasoning-Deltas und der
/// laufende Usage-Stand gehen live als [`TurnEvent`] hinaus. `None`, wenn
/// niemand zuhört (dann spart sich der Provider auch das SSE-Parsing nicht,
/// aber der Turn-Loop fordert kein Streaming an).
fn stream_sink_for_round(
    session: &AgentSession,
    turn_id: &harw_types::TurnId,
    turn_usage_so_far: &harw_types::TokenUsage,
) -> Option<crate::stream::StreamSink> {
    let emitter = session.live_emitter();
    if !emitter.is_observed() {
        return None;
    }
    let turn_id = turn_id.clone();
    let base = turn_usage_so_far.clone();
    Some(crate::stream::StreamSink::new(move |event| match event {
        crate::stream::ModelStreamEvent::TextDelta(text) if !text.is_empty() => {
            emitter.emit(TurnEvent::AssistantDelta {
                turn_id: turn_id.clone(),
                text,
            });
        }
        crate::stream::ModelStreamEvent::ReasoningDelta(text) if !text.is_empty() => {
            emitter.emit(TurnEvent::ReasoningDelta {
                turn_id: turn_id.clone(),
                text,
            });
        }
        crate::stream::ModelStreamEvent::Usage(round) => {
            let mut turn_total = base.clone();
            turn_total.add(&round);
            emitter.emit(TurnEvent::UsageUpdated {
                turn_id: turn_id.clone(),
                round,
                turn_total,
                final_round: false,
            });
        }
        _ => {}
    }))
}

/// Prüft einen `ToolCall` gegen alle `ApprovalHandler` und aggregiert.
///
/// # Beschreibung
/// **Jeder** registrierte Handler wird befragt, genau einmal und in
/// Registrierungsreihenfolge. Das Ergebnis folgt der strengsten Stimme:
///
/// `Deny` > `AskUser` > `Allow`
///
/// - Sagt irgendein Handler `Deny`, gilt das **erste** `Deny` (dessen
///   Begründung), auch wenn ein früherer Handler `AskUser` geliefert hat.
/// - Sonst gilt das **erste** `AskUser` samt seiner `ItemId`; spätere
///   `AskUser`-Anfragen werden verworfen.
/// - Nur wenn alle `Allow` sagen (oder kein Handler registriert ist), gilt
///   `Allow`.
///
/// Früher gewann der erste Nicht-`Allow`: ein vorn registriertes `AskUser`
/// (etwa `DefaultApprovalPolicy`) überstimmte ein später registriertes `Deny`
/// (etwa `ConfigApprovalPolicy`), der Nutzer konnte also freigeben, was eine
/// Politik verboten hatte (W1-05, Register G-004). Mit der Aggregation kann ein
/// angehängter Handler nur noch einschränken, nie lockern.
///
/// Ein `Deny` bricht die Befragung **nicht** ab: die Einmal-Befragung je
/// Handler bleibt die Invariante, ein Kurzschluss würde dagegen die
/// Befragungszahl von der Handler-Reihenfolge abhängig machen. `review` ist
/// laut Vertrag seiteneffektfrei (`harw_extension_api::ApprovalHandler`,
/// W0B-07); die weiteren Aufrufe öffnen deshalb keine zusätzlichen Prompts.
///
/// # Invariante
/// Ein `ApprovalHandler` wird pro Tool-Call **höchstens einmal** befragt: er
/// darf eine Approval-`ItemId` vergeben oder einen Audit-Eintrag schreiben.
/// Wer diese Funktion außerhalb des sequenziellen Tool-Loops aufruft (siehe
/// [`PreparedApprovals`]), muss die Entscheidung deshalb weiterreichen statt
/// sie später erneut einzuholen.
pub async fn check_approval(session: &AgentSession, call: &ToolCall) -> ApprovalDecision {
    let mut first_ask: Option<ApprovalDecision> = None;
    let mut first_deny: Option<ApprovalDecision> = None;
    for handler in session.registry().approval_handlers() {
        let decision = handler.review(call).await;
        match decision {
            ApprovalDecision::Allow => {}
            ApprovalDecision::AskUser(_) => {
                if first_ask.is_none() {
                    first_ask = Some(decision);
                }
            }
            ApprovalDecision::Deny(_) => {
                if first_deny.is_none() {
                    first_deny = Some(decision);
                }
            }
        }
    }
    first_deny.or(first_ask).unwrap_or(ApprovalDecision::Allow)
}

/// Guardrail-Entscheidungen, die für eine Modellantwort bereits eingeholt
/// wurden, aber noch nicht ausgewertet sind.
///
/// # Beschreibung
/// Der Parallel-Pfad muss jeden Call **vor** dem Start prüfen — sonst würde ein
/// `Deny` erst bemerkt, wenn das Werkzeug bereits läuft. Der sequenzielle Pfad
/// prüft dagegen jeden Call an seiner natürlichen Stelle. Damit ein
/// `ApprovalHandler` nie zweimal zur selben Modellantwort befragt wird, legt die
/// Vorprüfung ihre Entscheidungen hier ab; der sequenzielle Fallback verbraucht
/// sie, statt erneut zu fragen (siehe [`check_approval`]).
///
/// Jeder Eintrag ist bereits die **aggregierte** Entscheidung aller Handler
/// (`Deny` > `AskUser` > `Allow`, siehe [`check_approval`]) — Vorprüfung und
/// sequenzieller Pfad entscheiden damit identisch. Die Vorprüfung bricht beim
/// ersten Call ab, dessen Aggregat nicht `Allow` ist. Für die dahinter
/// liegenden Calls steht deshalb kein Eintrag bereit — die werden im
/// sequenziellen Pfad ganz normal, also ebenfalls genau einmal, geprüft.
///
/// # Nebenläufigkeit
/// Reiner Datenhalter ohne innere Veränderlichkeit; lebt genau eine
/// Modellantwort lang auf dem Stack von `drive_turn`.
#[derive(Debug, Default)]
struct PreparedApprovals {
    /// Entscheidung je Call-Position der Modellantwort. `None` heißt „noch
    /// nicht gefragt", ein verbrauchter Eintrag wird wieder zu `None`.
    decisions: Vec<Option<ApprovalDecision>>,
}

impl PreparedApprovals {
    /// Entnimmt die für `index` bereits eingeholte Entscheidung.
    ///
    /// # Returns
    /// `Some(decision)`, wenn die Vorprüfung diesen Call schon geprüft hat —
    /// der Eintrag ist danach verbraucht. `None`, wenn der Aufrufer selbst
    /// fragen muss.
    fn take(&mut self, index: usize) -> Option<ApprovalDecision> {
        self.decisions.get_mut(index).and_then(Option::take)
    }
}

/// Liest die optionale Leitfrage eines Handoff-Calls aus dessen Argumenten.
///
/// # Beschreibung
/// Nur ein JSON-String unter dem Schlüssel `"question"` zählt. Jede andere Form
/// (Objekt, Zahl, fehlender Schlüssel) ergibt `None`: die Argumente stammen vom
/// Modell, und ein Ereignisfeld darf keinen Wert behaupten, den das Modell so
/// nicht geliefert hat.
///
/// # Arguments
/// - `arguments` (`&serde_json::Value`): die Argumente des Handoff-Calls.
///
/// # Returns
/// `Some(frage)` als eigener `String`, sonst `None`.
fn child_question(arguments: &serde_json::Value) -> Option<String> {
    arguments
        .get("question")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

/// Sammelt alle Tool-Specs über alle `ToolProvider` und filtert nach der
/// session-level [`SessionActivation`][crate::activation::SessionActivation].
///
/// # Description
/// Iterates every registered `ToolProvider`, collects their [`ToolSpec`]s, and
/// removes any tool whose name is disabled by the session activation. Duplicate
/// tool names across providers are still rejected with
/// [`CoreError::DuplicateTool`] regardless of activation state (a disabled tool
/// that shares a name with an enabled one is still a registry error).
///
/// # Arguments
/// - `session` (`&AgentSession`): session providing both the registry and the
///   activation filter.
///
/// # Returns
/// Filtered, de-duplicated list of [`ToolSpec`]s, or a
/// [`CoreError::DuplicateTool`] if any tool name appears more than once.
///
/// # Errors
/// - [`CoreError::DuplicateTool`]: two or more providers registered a tool
///   with the same name.
pub fn collect_tools(session: &AgentSession) -> CoreResult<Vec<ToolSpec>> {
    let activation = session.activation();
    let mut specs = Vec::new();
    let mut names = BTreeSet::new();
    for provider in session.registry().tool_providers() {
        for spec in provider.tools() {
            let name = match &spec {
                ToolSpec::Function(function) => function.name.as_str(),
            };
            if !names.insert(name.to_owned()) {
                return Err(CoreError::DuplicateTool {
                    name: name.to_owned(),
                });
            }
            let tool_name = match &spec {
                ToolSpec::Function(f) => &f.name,
            };
            if activation.is_tool_enabled(tool_name) {
                specs.push(spec);
            }
        }
    }
    // Stabil nach Tool-Namen sortieren: Provider-Prompt-Caches brauchen ein
    // byte-identisches Tool-Array über Runden hinweg; die Registrierungs-
    // reihenfolge der `ToolProvider` ist dafür kein verlässliches Kriterium.
    specs.sort_by(|a, b| a.name().cmp(b.name()));
    Ok(specs)
}

/// Sucht den zuständigen `ToolExecutor` für einen Tool-Namen.
///
/// # Description
/// Returns a cloned `Arc` so no borrow on the session is held across the
/// (async) tool execution boundary. Returns `None` if the tool is disabled by
/// the session's [`SessionActivation`][crate::activation::SessionActivation] or
/// if no provider claims the tool.
///
/// # Arguments
/// - `session` (`&AgentSession`): session providing both the registry and the
///   activation filter.
/// - `name` (`&ToolName`): the tool to look up.
///
/// # Returns
/// `Some(executor)` if the tool is enabled and a provider declares an executor
/// for it; `None` otherwise.
///
/// # Concurrency
/// Synchronous; safe to call from any thread while holding a shared reference
/// to the session.
pub fn find_executor(session: &AgentSession, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
    if !session.activation().is_tool_enabled(name) {
        return None;
    }
    session
        .registry()
        .tool_providers()
        .iter()
        .find_map(|p| p.executor(name))
}

/// Resolve an executor only when its provider explicitly declares the tool
/// safe for concurrent execution.
///
/// # Description
/// Like [`find_executor`] but additionally requires the provider to declare
/// the tool as `parallel_safe`. Also checks the session's
/// [`SessionActivation`][crate::activation::SessionActivation]: a disabled tool
/// is never returned even if the provider marks it parallel-safe.
///
/// # Arguments
/// - `session` (`&AgentSession`): session providing both the registry and the
///   activation filter.
/// - `name` (`&ToolName`): the tool to look up.
///
/// # Returns
/// `Some(executor)` if the tool is enabled, registered, and parallel-safe;
/// `None` otherwise.
///
/// # Concurrency
/// Synchronous; safe to call from any thread while holding a shared reference
/// to the session.
pub fn find_parallel_executor(
    session: &AgentSession,
    name: &ToolName,
) -> Option<Arc<dyn ToolExecutor>> {
    if !session.activation().is_tool_enabled(name) {
        return None;
    }
    session
        .registry()
        .tool_providers()
        .iter()
        .find_map(|provider| {
            provider
                .parallel_safe(name)
                .then(|| provider.executor(name))
                .flatten()
        })
}

/// Sucht den registrierten `context.load`-Ausführer über [`find_executor`] und
/// belegt sein Kassenbuch für den laufenden Turn vor, sofern die Sitzung ein
/// `ContextProgram` deklariert.
///
/// # Beschreibung
///
/// Schließt den in der Moduldoku (§ "Zweiter Nachtrag", Punkt 4) benannten
/// Weg: von einem echten Eintrittspunkt — [`drive_turn`], vor dem ersten
/// Model-Aufruf eines Turns — über [`find_executor`] und die neue
/// `harw_tools::ToolExecutor::as_context_load_executor`-Methode zu einer
/// konkreten [`harw_tools::ContextLoadExecutor`]-Instanz, und über
/// [`AgentSession::context_program`] zum deklarierten `ContextProgram`
/// derselben Sitzung. **Beide Hälften sind damit von hier aus erreichbar** —
/// die Frage aus dem Knotenauftrag ist positiv beantwortet.
///
/// Aufgerufen wird `seed_turn` mit einer **leeren** `spent_per_section`-Map,
/// nicht mit tatsächlich verbrauchten Kosten je Sektion:
/// [`gather_context`] läuft weiterhin über den alten
/// `Vec<harw_extension_api::ContextFragment>`-Pfad (§ Moduldoku, "Zweiter
/// Nachtrag", Punkt 3) — die neue, `harw_context::Fragment`-bewusste Montage
/// (`crate::context_budget::Assembly<..>`), die tatsächliche Kosten je
/// Sektion kennen würde, läuft in dieser Datei nicht. Eine leere Map ist
/// deshalb keine Vereinfachung, sondern die exakte Angabe: die Montage
/// dieses Turns hat im neuen Kosten-Schema bislang nichts ausgegeben, weil
/// sie das neue Kosten-Schema noch nicht bedient. **Hier bricht die Kette zu
/// `DetailMode::References` deshalb weiterhin** — nicht mehr am fehlenden
/// Weg zum Ausführer (das schließt diese Funktion), sondern unverändert am
/// fehlenden `ContextProvider`, der `harw_context::Fragment` produziert
/// (Blocker 1 der Moduldoku, außerhalb dieses Schreibbereichs). Sobald dieser
/// Provider existiert, ersetzt die tatsächliche `spent_per_section` diese
/// leere Map.
///
/// Eine Sitzung ohne deklariertes `ContextProgram`
/// (`AgentSession::context_program() == None`) bricht diese Funktion sofort
/// ab, ohne `find_executor` überhaupt aufzurufen, und verhält sich damit
/// unverändert — die Auflage, unter der der Vorgängerknoten
/// `with_context_program` eingeführt hat, bleibt gewahrt. Ebenso ein no-op,
/// wenn `context.load` gar nicht registriert oder per
/// `SessionActivation` deaktiviert ist ([`find_executor`] liefert dann
/// `None`).
///
/// # Arguments
/// - `session` (`&AgentSession`): liefert sowohl das optionale
///   `ContextProgram` als auch (über [`find_executor`]) den registrierten
///   `context.load`-Ausführer.
/// - `turn_id` (`&harw_types::TurnId`): der Turn, dessen Kassenbuch vorbelegt
///   wird.
///
/// # Concurrency
/// Synchron; `ContextLoadExecutor::seed_turn` serialisiert intern über ein
/// `Mutex`.
fn seed_context_load_ledger(session: &AgentSession, turn_id: &harw_types::TurnId) {
    if session.context_program().is_none() {
        return;
    }
    let tool_name = ToolName::new(harw_tools::CONTEXT_LOAD_TOOL_NAME);
    let Some(executor) = find_executor(session, &tool_name) else {
        return;
    };
    let Some(context_load_executor) = executor.as_context_load_executor() else {
        return;
    };
    let spent_per_section: BTreeMap<harw_context::SectionName, u32> = BTreeMap::new();
    context_load_executor.seed_turn(turn_id.clone(), spent_per_section);
}

/// Erkennt, ob ein Tool-Call ein Handoff ist (`transfer_to_<role>`).
#[must_use]
pub fn handoff_role(name: &ToolName) -> Option<String> {
    name.as_str()
        .strip_prefix(HANDOFF_PREFIX)
        .filter(|rest| !rest.is_empty())
        .map(ToOwned::to_owned)
}

/// Übersetzt eine `ToolOutput` in das explizite Wire-Ergebnis eines
/// `ToolResultItem`.
fn output_to_result(output: ToolOutput) -> ToolCallResult {
    match output {
        ToolOutput::Text { content } => ToolCallResult::success(serde_json::Value::String(content)),
        ToolOutput::Json { content } => ToolCallResult::success(content),
        ToolOutput::Error { message } => ToolCallResult::error(message),
    }
}

/// Meldet das Ergebnis eines ausgeführten Tool-Aufrufs an den optionalen
/// [`crate::capture::ToolOutcomeObserver`] der Session (Projektgedächtnis-
/// Erfassung, Addendum B).
///
/// # Beschreibung
/// No-op, wenn keine Session einen Beobachter registriert hat (der
/// Normalfall). Der `Arc` wird vor dem Aufruf geklont, um Borrow-Konflikte
/// mit dem übrigen `session`-Zugriff an den Aufrufstellen zu vermeiden.
/// `output_text` ist der reine Text bei einem Text-Erfolg, die kompakte
/// JSON-Form (`serde_json::to_string`, Fallback leerer String) bei einem
/// strukturierten Erfolg, und die Fehlermeldung bei `ToolCallResult::Error`.
/// Wird bewusst **nicht** für abgebrochene (`cancelled`) Turns aufgerufen —
/// die jeweiligen Aufrufstellen lassen diesen Pfad aus.
///
/// # Arguments
/// - `session` (`&AgentSession`): liefert Session-ID und den optionalen
///   Beobachter.
/// - `tool_name` (`&str`): Name des aufgerufenen Tools.
/// - `arguments` (`&serde_json::Value`): die vom Modell übergebenen Argumente.
/// - `result` (`&ToolCallResult`): das endgültige, bereits bekannte Ergebnis.
///
/// # Concurrency
/// Synchron; ruft `ToolOutcomeObserver::on_tool_outcome` direkt aus dem
/// Turn-Loop-Pfad auf. Beobachter müssen billig sein und dürfen nicht
/// fehlschlagen.
fn notify_tool_outcome(
    session: &AgentSession,
    tool_name: &str,
    arguments: &serde_json::Value,
    result: &ToolCallResult,
) {
    emit_plan_update(session, tool_name, arguments, result);
    let Some(observer) = session.tool_outcome_observer().cloned() else {
        return;
    };
    let (status, output_text) = tool_outcome_parts(result);
    observer.on_tool_outcome(
        session.id(),
        &ToolOutcome {
            tool_name,
            arguments,
            status,
            output_text: &output_text,
        },
    );
}

/// Meldet [`TurnEvent::PlanUpdated`] nach einem erfolgreichen Aufruf des
/// Plan-Werkzeugs (Operation `plan` als Modell-Tool). Plan-ID
/// und Revision stammen aus dem Ergebnis, sofern es sie trägt, sonst aus den
/// Argumenten; die Zusammenfassung ist die ausgeführte Aktion.
fn emit_plan_update(
    session: &AgentSession,
    tool_name: &str,
    arguments: &serde_json::Value,
    result: &ToolCallResult,
) {
    let is_plan_tool = tool_name == "plan";
    let ToolCallResult::Success { value } = result else {
        return;
    };
    if !is_plan_tool {
        return;
    }
    let lookup = |key: &str| {
        value
            .get(key)
            .or_else(|| value.get("data").and_then(|data| data.get(key)))
            .or_else(|| arguments.get(key))
    };
    let plan_id = lookup("plan_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("current")
        .to_owned();
    let revision = lookup("revision")
        .and_then(|rev| {
            rev.as_u64()
                .or_else(|| rev.as_str().and_then(|text| text.parse().ok()))
        })
        .unwrap_or(0);
    let summary = arguments
        .get("action")
        .or_else(|| arguments.get("command"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Plan aktualisiert")
        .to_owned();
    emit(
        session,
        TurnEvent::PlanUpdated {
            plan_id,
            revision,
            summary,
        },
    );
}

/// Benachrichtigt den optionalen [`crate::guard::ProgressObserver`] der
/// Session, dass gerade Fortschritt stattgefunden hat (Addendum F+G —
/// Lease-Erneuerung durch `ManagedAgentSpawner`).
///
/// # Beschreibung
/// Wird nach jeder Modellrunde und nach jedem — auch abgelehnten oder
/// fehlgeschlagenen — Tool-Ergebnis aufgerufen: schon der Versuch zeigt, dass
/// die Session noch lebt, unabhängig vom Erfolg des einzelnen Aufrufs.
fn notify_progress(session: &AgentSession) {
    if let Some(observer) = session.progress_observer() {
        observer.on_progress(session.id());
    }
}

/// Wie [`notify_progress`], nach einem Tool-Ergebnis: zählt zusätzlich den
/// Aufruf für die Live-Beobachtung.
fn notify_tool_progress(session: &AgentSession) {
    if let Some(observer) = session.progress_observer() {
        observer.on_tool_call(session.id());
    }
    notify_progress(session);
}

/// Wie [`notify_progress`], nach einer Modell-Runde: verbucht zusätzlich
/// deren Token-Nutzung.
fn notify_round_progress(session: &AgentSession, usage: &harw_types::TokenUsage) {
    if let Some(observer) = session.progress_observer() {
        observer.on_round_usage(session.id(), usage);
    }
    notify_progress(session);
}

// Status + Textform eines `ToolCallResult`, wie sie sowohl
// `notify_tool_outcome` als auch die Turn-Wächter (Addendum F+G) brauchen —
// gleiche Ableitungsregel wie in `notify_tool_outcome`s Doku beschrieben.
fn tool_outcome_parts(result: &ToolCallResult) -> (ToolOutcomeStatus, String) {
    match result {
        ToolCallResult::Success { value } => {
            let text = match value {
                serde_json::Value::String(text) => text.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            (ToolOutcomeStatus::Success, text)
        }
        ToolCallResult::Error { message } => (ToolOutcomeStatus::Error, message.clone()),
    }
}

/// Befragt den optionalen [`crate::guard::PitfallAdvisor`] der Session vor
/// einer Werkzeugausführung (Addendum F+G).
///
/// # Returns
/// `None`, wenn keine Beratung registriert ist oder kein Pitfall zutrifft.
/// Sonst ein bereits mit `[harw-Wächter] ` präfixierter Hinweistext, den der
/// Aufrufer an das Ergebnis **desselben** Aufrufs anhängen soll — der Treffer
/// selbst ist bereits über [`report_drift`] gemeldet.
async fn apply_pitfall_advice(
    session: &AgentSession,
    store: &dyn StateStore,
    tool_name: &str,
    arguments: &serde_json::Value,
) -> Option<String> {
    let advisor = session.pitfall_advisor()?.clone();
    let hint = advisor.advise(tool_name, arguments)?;
    let event = DriftEvent {
        kind: DriftKind::PitfallMatch,
        session_id: session.id().to_string(),
        detail: format!("Pitfall-Treffer für `{tool_name}`"),
        tool_name: Some(tool_name.to_owned()),
        child_role: None,
    };
    report_drift(session, store, &event).await;
    Some(format!("[harw-Wächter] {hint}"))
}

/// Wendet die Turn-Wächter (Addendum F+G) auf ein einzelnes, bereits
/// berechnetes Tool-Ergebnis an.
///
/// # Beschreibung
/// No-op (liefert `None`), wenn kein [`TurnGuard`] aktiv ist. Sonst:
/// vermerkt eine neue erfolgreiche Signatur für die Fortschritts-Erkennung
/// der Runde (`round_progressed`), wertet
/// [`TurnGuard::observe_tool_result`] aus und hängt bei `Warn` den
/// kombinierten Hinweis (ein ggf. aus einer vorherigen Runde übrig
/// gebliebener `pending_hint` plus der neue Hinweis) über [`append_hint`] an
/// `result` an. Ein Hinweis, der keinem Aufruf dieser Runde mehr zugeordnet
/// werden kann, bleibt in `pending_hint` für das nächste Tool-Ergebnis des
/// Turns stehen.
///
/// Für [`crate::guard::POLLING_TOOLS`] zählt ein wiederholter *erfolgreicher*
/// Aufruf mit bereits gesehener Signatur trotzdem als Rundenfortschritt
/// (Fix C, Moduldoku „Achter Nachtrag") — legitimes Polling eines laufenden
/// Hintergrundprozesses soll nicht als Stillstand erkannt werden.
///
/// # Returns
/// `None`, wenn der Turn weiterläuft; `Some(reason)`, wenn ein `Abort`-Befund
/// den Turn beenden muss — der Aufrufer beendet ihn dann wie bei
/// ausgeschöpftem `max_model_rounds` (siehe [`cancel_turn`]/
/// [`cancel_turn_with_pending_calls`]), ohne eine neue `CoreError`-Variante.
#[allow(clippy::too_many_arguments)]
async fn apply_tool_guard(
    session: &AgentSession,
    store: &dyn StateStore,
    guard: Option<&mut TurnGuard>,
    seen_success_signatures: &mut HashSet<String>,
    pending_hint: &mut Option<String>,
    round_progressed: &mut bool,
    tool_name: &str,
    arguments: &serde_json::Value,
    result: &mut ToolCallResult,
) -> Option<CancelReason> {
    let guard = guard?;
    let (status, output_text) = tool_outcome_parts(result);
    if status == ToolOutcomeStatus::Success {
        let signature = crate::guard::call_signature(tool_name, arguments);
        // Fix C (Moduldoku „Achter Nachtrag"): eine bereits gesehene Signatur
        // zählt normalerweise nicht erneut als Fortschritt — für
        // `crate::guard::POLLING_TOOLS` (z. B. `shell.exec`) ist ein
        // wiederholter *erfolgreicher* Aufruf mit identischen Argumenten
        // jedoch legitimes Warten auf einen laufenden Hintergrundprozess,
        // kein Stillstand, und zählt deshalb unabhängig davon, ob die
        // Signatur neu ist.
        let is_polling_tool = crate::guard::POLLING_TOOLS.contains(&tool_name);
        if seen_success_signatures.insert(signature) || is_polling_tool {
            *round_progressed = true;
        }
    }
    let verdict = guard.observe_tool_result(tool_name, arguments, status, &output_text);
    let mut hint_to_apply = pending_hint.take();
    let abort_reason = match verdict {
        GuardVerdict::Continue => None,
        GuardVerdict::Warn { event, hint } => {
            report_drift(session, store, &event).await;
            hint_to_apply = Some(match hint_to_apply {
                Some(existing) => format!("{existing}\n\n{hint}"),
                None => hint,
            });
            None
        }
        GuardVerdict::Abort { event, hint } => {
            report_drift(session, store, &event).await;
            tracing::warn!(hint = %hint, "turn_loop.guard_abort");
            Some(CancelReason::Budget)
        }
    };
    if let Some(hint) = hint_to_apply {
        append_hint(result, &hint);
    }
    abort_reason
}

/// Meldet ein einzelnes erkanntes Drift-Ereignis an `StateStore::record_drift`
/// und den optionalen [`crate::guard::DriftObserver`] der Session (Addendum
/// F+G).
///
/// # Beschreibung
/// Beide Meldewege laufen unabhängig voneinander: ein fehlschlagender
/// `record_drift`-Aufruf (Persistenz) wird nur geloggt (`tracing::warn!`) und
/// verhindert nicht, dass der synchrone [`crate::guard::DriftObserver`]
/// trotzdem benachrichtigt wird — dieselbe Best-Effort-Haltung wie
/// `StateStore::record_usage`.
///
/// # Concurrency
/// `async` nur wegen `store.record_drift`; der Beobachter-Aufruf selbst ist
/// synchron.
async fn report_drift(session: &AgentSession, store: &dyn StateStore, event: &DriftEvent) {
    if let Err(error) = store.record_drift(session.id(), event).await {
        tracing::warn!(%error, kind = event.kind.key(), "turn_loop.record_drift_failed");
    }
    if let Some(observer) = session.drift_observer() {
        observer.on_drift(event);
    }
}

/// Hängt einen Wächter-Hinweis (Addendum F+G, Präfix `[harw-Wächter] `) an ein
/// [`ToolCallResult`] an, statt es zu ersetzen — das Modell sieht das
/// tatsächliche Tool-Ergebnis weiterhin vollständig.
///
/// # Arguments
/// - `result` (`&mut ToolCallResult`): wird um `hint` erweitert (String-Erfolg
///   bzw. Fehlermeldung bekommt `hint` angehängt; ein strukturierter
///   Erfolgswert wird dafür zu seiner kompakten JSON-Textform samt `hint`).
/// - `hint` (`&str`): bereits fertig formatierter Hinweistext.
fn append_hint(result: &mut ToolCallResult, hint: &str) {
    match result {
        ToolCallResult::Success { value } => match &mut *value {
            serde_json::Value::String(text) => {
                text.push_str("\n\n");
                text.push_str(hint);
            }
            other => {
                let rendered = serde_json::to_string(other).unwrap_or_default();
                *value = serde_json::Value::String(format!("{rendered}\n\n{hint}"));
            }
        },
        ToolCallResult::Error { message } => {
            message.push_str("\n\n");
            message.push_str(hint);
        }
    }
}

/// Builds the authority passed across the final tool-execution boundary.
///
/// This is deliberately derived from session state rather than `TurnInput`
/// or `ToolCall.arguments`: both of those can originate with untrusted model
/// or channel input. A session with tools but no resolved sandbox is rejected
/// instead of falling back to ambient host permissions.
// `cancel` (W3/C-CANCEL): the turn's `CancelToken`, attached to the built
// `ToolExecutionContext` so a long-running executor (process, MCP,
// context-load) can observe cancellation mid-call — see
// `ToolExecutionContext::with_cancel`'s doc.
fn tool_execution_context(
    session: &AgentSession,
    ctx: &TurnInputContext,
    cancel: &CancelToken,
) -> CoreResult<ToolExecutionContext> {
    let sandbox = session
        .spawn_context()
        .map(|context| context.sandbox.clone())
        .ok_or_else(|| CoreError::MissingToolExecutionContext {
            session_id: session.id().to_string(),
        })?;
    Ok(
        ToolExecutionContext::new(ctx.session_id.clone(), ctx.turn_id.clone(), sandbox)
            .with_cancel(cancel.clone()),
    )
}

/// Converts the one recoverable authority-boundary rejection into a result
/// the requesting model can observe and react to. The typed error is retained
/// for every other caller; no executor is ever reached without this context.
fn missing_tool_execution_context_result(error: CoreError) -> CoreResult<ToolCallResult> {
    match error {
        missing @ CoreError::MissingToolExecutionContext { .. } => {
            Ok(ToolCallResult::error(missing.to_string()))
        }
        other => Err(other),
    }
}

/// Retrieves immutable child authority. Handoffs are also execution: allowing
/// one without a resolved parent context would let a legacy spawner create a
/// child with ambient permissions.
fn governed_spawn_context(session: &AgentSession) -> CoreResult<SpawnContext> {
    session
        .spawn_context()
        .cloned()
        .ok_or_else(|| CoreError::MissingToolExecutionContext {
            session_id: session.id().to_string(),
        })
}

// ---------------------------------------------------------------------------
// Der eigentliche Turn-Loop
// ---------------------------------------------------------------------------

/// Führt einen kompletten Turn aus: von `Idle` über den Model-/Tool-Zyklus bis
/// zum Turn-Ende — oder bis zu einem Handoff-/Approval-Pausepunkt.
///
/// Schritte: Context → Instructions → Observer → Model-Call → Tool-Call-Loop
/// (Guardrail → Handoff-Detection → Execute → ToolResult → next Model-Call)
/// → Turn-Ende. A normal `ToolExecutor`, including one that wraps a bounded
/// micro-agent, returns its output to this requesting session as a
/// `ToolResult`; the loop then invokes this same session's model again. A
/// `transfer_to_*` call is deliberately different: it transfers ownership and
/// pauses this session for [`resume_after_child`].
pub async fn run_turn(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    input: TurnInput,
) -> CoreResult<TurnOutcome> {
    run_turn_with_approvals(session, model, store, None, input).await
}

/// Runs a turn while durably recording every approval request before it is
/// exposed to a channel. Production channels must use this entrypoint; the
/// plain [`run_turn`] function remains a lightweight test/demo seam.
pub async fn run_turn_durable(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: &ApprovalStore,
    input: TurnInput,
) -> CoreResult<TurnOutcome> {
    run_turn_with_approvals(session, model, store, Some(approvals), input).await
}

async fn run_turn_with_approvals(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: Option<&ApprovalStore>,
    input: TurnInput,
) -> CoreResult<TurnOutcome> {
    let handle = session
        .try_start_turn()
        .map_err(|r| CoreError::TurnRejected(r.to_string()))?;
    let turn_id = handle.turn_id.clone();
    let session_id = handle.session_id.clone();
    // Siehe Moduldoku „Siebter Nachtrag": der von `TurnInput::with_control`
    // gesetzte Steuerblock wird hier zusätzlich auf der Session hinterlegt
    // (`AgentSession::set_active_turn_control`), damit `resume_after_child`/
    // `resume_after_approval` ihn nach einer Handoff-/Rückfrage-Pause
    // zurücklesen können, statt sich einen frischen, unbegrenzten Block zu
    // bauen.
    // Ein Aufrufer ohne eigene Grenzen (TUI, Gateway: `TurnControl::new()`)
    // bekommt die Vorgabe-Grenzen der Session (aus dem Budget des Laufs,
    // `AgentSession::with_default_turn_limits`); explizit gesetzte Grenzen
    // haben Vorrang.
    let control = match session.default_turn_limits() {
        Some(limits) if *input.control.limits() == TurnLimits::unlimited() => {
            input.control.clone().with_limits(limits)
        }
        _ => input.control.clone(),
    };
    session.set_active_turn_control(control.clone());

    // Outer span covering the entire turn's lifecycle.
    let turn_span = tracing::info_span!(
        "agent.turn",
        turn_id = %turn_id,
        session_id = %session_id,
    );
    let _turn_guard = turn_span.enter();

    let ctx = TurnInputContext {
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        metadata: input.metadata,
    };

    // A freshly-created in-memory session may be resuming a durable session.
    // Hydrate it before appending the new user item so the model sees the
    // complete transcript and the live session remains the single append
    // authority for the rest of this turn.
    if input.user_text.is_some() && session.history().is_empty() {
        let history = match store.load_history(session.id()).await {
            Ok(history) => history,
            Err(error) => {
                let error = state_store_error(error, "history load");
                transition_after_turn_failure(session, &ctx, &error);
                return Err(error);
            }
        };
        *session.history_mut() = history;
    }

    // User-Input in den Verlauf spielen und persistieren.
    if let Some(text) = input.user_text {
        session.history_mut().push_user_text(text);
        if let Err(error) = persist_last(session, store).await {
            transition_after_turn_failure(session, &ctx, &error);
            return Err(error);
        }
    }

    // Addendum D: harte Verdichtung am Beginn eines neuen Auftrags-Turns
    // (Orchestrator-Schicht) — vor der ersten Modellrunde dieses Turns, aber
    // nach dem Einspielen des User-Inputs, damit die Schätzung den vollen,
    // aktuellen Verlauf sieht. `resume_after_child`/`resume_after_approval`
    // laufen nicht durch diese Funktion und sind damit keine Auftragsgrenze.
    maybe_hard_compact_at_turn_start(session, model, store).await;

    notify_turn_start(
        session,
        &TurnStartInput {
            session_id,
            turn_id,
        },
    );

    let result = drive_turn(session, model, store, approvals, &ctx, handle, control).await;
    if let Err(error) = &result {
        transition_after_turn_failure(session, &ctx, error);
    }
    result
}

/// Returns the session to `Idle` after a provider-declared retryable failure,
/// so a caller can submit the same valid session again. All other failures are
/// terminal because they may indicate a broken invariant or unsafe execution
/// boundary.
///
/// # Description
/// Retryability is delegated to [`crate::model::ModelError::is_retryable`]
/// rather than hardcoded here: any [`CoreError::Model`] whose inner
/// [`crate::model::ModelError`] reports itself as retryable (currently
/// `Transient`, `Timeout`, `RateLimited`) returns the session to `Idle`.
/// Everything else — including non-`Model` `CoreError` variants and
/// non-retryable `ModelError` variants such as `Refusal` or `Auth` — calls
/// [`AgentSession::fail`], moving the session to the terminal `Failed` state.
fn transition_after_turn_failure(
    session: &mut AgentSession,
    ctx: &TurnInputContext,
    error: &CoreError,
) {
    let retryable = matches!(
        error,
        CoreError::Model(model_error) if model_error.is_retryable()
    );

    if retryable {
        // `transition_after_turn_failure` liefert `()` zurück, ein `?` ist
        // hier also nicht möglich — der Fehler wird stattdessen mit vollem
        // Kontext protokolliert. Die Session bleibt in diesem seltenen Fall
        // in ihrem bisherigen Zustand hängen (weder `Idle` noch `Failed`),
        // statt einen zweiten, hier nicht belegbaren Fehler zu erfinden.
        if let Err(complete_error) = session.complete_turn(
            TurnHandle {
                turn_id: ctx.turn_id.clone(),
                session_id: ctx.session_id.clone(),
            },
            harw_types::TokenUsage::default(),
        ) {
            tracing::error!(
                turn_id = %ctx.turn_id,
                session_id = %ctx.session_id,
                original_error = %error,
                complete_error = %complete_error,
                "turn.retry_complete_failed",
            );
        }
    } else {
        session.fail(error.to_string());
    }

    // Der Fehler wird Teil des Verlaufs (sichtbar in Export/Resume) und
    // zusätzlich als Session-Ereignis gemeldet.
    session
        .history_mut()
        .push_error(error.to_string(), retryable);
    session.report_error(error.to_string(), retryable);

    emit(
        session,
        TurnEvent::TurnFailed {
            turn_id: ctx.turn_id.clone(),
            reason: error.to_string(),
            retryable,
        },
    );
}

/// Nimmt einen pausierten Turn nach Abschluss eines Child-Handoffs wieder auf.
///
/// Das Child-Ergebnis wird als `ToolResult` in den Eltern-Verlauf gespielt
/// (Agents-SDK-Handoff-Semantik), dann läuft der Model-/Tool-Zyklus weiter.
pub async fn resume_after_child(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    child: SessionId,
    call_id: ToolCallId,
    child_result: ToolCallResult,
) -> CoreResult<TurnOutcome> {
    resume_after_child_with_approvals(session, model, store, None, child, call_id, child_result)
        .await
}

/// Durable counterpart to [`resume_after_child`]. Use it when the parent was
/// started through [`run_turn_durable`] so any subsequent approval pause is
/// persisted in the same authority store.
pub async fn resume_after_child_durable(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: &ApprovalStore,
    child: SessionId,
    call_id: ToolCallId,
    child_result: ToolCallResult,
) -> CoreResult<TurnOutcome> {
    resume_after_child_with_approvals(
        session,
        model,
        store,
        Some(approvals),
        child,
        call_id,
        child_result,
    )
    .await
}

async fn resume_after_child_with_approvals(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: Option<&ApprovalStore>,
    child: SessionId,
    call_id: ToolCallId,
    child_result: ToolCallResult,
) -> CoreResult<TurnOutcome> {
    let turn_id = session
        .current_turn()
        .cloned()
        .ok_or_else(|| CoreError::TurnRejected("resume without an active turn".to_owned()))?;
    let session_id = session.id().clone();

    // Re-enter the same logical turn span after child completion.
    let turn_span = tracing::info_span!(
        "agent.turn",
        turn_id = %turn_id,
        session_id = %session_id,
    );
    let _turn_guard = turn_span.enter();

    let ctx = TurnInputContext {
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        metadata: serde_json::Value::Null,
    };

    let call_id_for_completion = call_id.clone();
    let child_outcome = match &child_result {
        ToolCallResult::Success { .. } => "completed",
        _ => "failed",
    };
    let child_duration_ms = session.take_handoff_elapsed_ms();
    session
        .history_mut()
        .push_tool_result(call_id, child_result, 0);
    if let Err(error) = persist_last(session, store).await {
        transition_after_turn_failure(session, &ctx, &error);
        return Err(error);
    }

    // Do not release the child's admission until its terminal ToolResult is
    // durable in the parent transcript. A failed write leaves the lease intact
    // for recovery. The durable path lets the spawner durably complete that
    // lease before releasing it; the legacy path retains child_finished.
    session.child_completed(&child, &call_id_for_completion)?;

    // Erst hier melden: das Ergebnis ist persistiert *und* die Session hat die
    // Korrelation gegen ihren offenen Handoff bestätigt. Vorher wäre es die
    // Meldung eines Kindes, das gar nicht das erwartete sein muss.
    //
    // `duration_ms` misst ab `begin_handoff` (Wanduhr des Eltern-Turns);
    // `outcome` folgt dem Ergebnis-Typ des Kindes.
    emit(
        session,
        TurnEvent::ChildCompleted {
            turn_id: turn_id.clone(),
            child: child.clone(),
            outcome: child_outcome.to_owned(),
            duration_ms: child_duration_ms,
        },
    );
    if let Some(spawner) = session.registry().spawner() {
        if approvals.is_some() {
            let completion = spawner
                .child_completed(&child, jiff::Timestamp::now())
                .map_err(|error| CoreError::HandoffFailed {
                    role: "child completion".to_owned(),
                    reason: error.to_string(),
                });
            if let Err(error) = completion {
                transition_after_turn_failure(session, &ctx, &error);
                return Err(error);
            }
        } else {
            spawner.child_finished(&child);
        }
    }

    let handle = TurnHandle {
        turn_id,
        session_id,
    };

    // Moduldoku „Siebter Nachtrag": der Steuerblock des ursprünglichen Turns
    // überlebt jetzt die Handoff-Pause auf der Session
    // (`AgentSession::set_active_turn_control`) — nur wenn dort wider
    // Erwarten nichts hinterlegt ist (Session nie über `run_turn`/
    // `run_turn_durable` gestartet, z. B. direkter `try_start_turn` in
    // Tests), fällt dies auf einen frischen, unbegrenzten Block zurück.
    let control = session
        .active_turn_control()
        .cloned()
        .unwrap_or_else(TurnControl::new);
    let result = drive_turn(session, model, store, approvals, &ctx, handle, control).await;
    if let Err(error) = &result {
        transition_after_turn_failure(session, &ctx, error);
    }
    result
}

/// Resume an approval pause using the exact tool call captured by the session.
/// This prevents an approval callback from substituting different arguments.
pub async fn resume_after_approval(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    actor: ApprovalActor,
    resolution: ApprovalResolution,
) -> CoreResult<TurnOutcome> {
    resume_after_approval_with_store(session, model, store, None, actor, resolution).await
}

/// Resumes an approval only after atomically consuming the durable request.
/// Duplicate callbacks, mismatched actors, and post-restart replays therefore
/// cannot execute the stored tool call a second time.
pub async fn resume_after_approval_durable(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: &ApprovalStore,
    actor: ApprovalActor,
    resolution: ApprovalResolution,
) -> CoreResult<TurnOutcome> {
    resume_after_approval_with_store(session, model, store, Some(approvals), actor, resolution)
        .await
}

async fn resume_after_approval_with_store(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: Option<&ApprovalStore>,
    actor: ApprovalActor,
    resolution: ApprovalResolution,
) -> CoreResult<TurnOutcome> {
    if let Some(approvals) = approvals {
        let pending = session.pending_approval().ok_or_else(|| {
            CoreError::TurnRejected("durable approval resume without a pending request".to_owned())
        })?;
        let decision = match &resolution {
            ApprovalResolution::Approve => ReviewDecision::Approved,
            ApprovalResolution::Reject { .. } => ReviewDecision::Rejected,
        };
        let comment = match &resolution {
            ApprovalResolution::Approve => None,
            ApprovalResolution::Reject { reason } => Some(reason.clone()),
        };
        approvals.resolve(
            session.id(),
            &pending.request,
            decision,
            comment,
            &actor,
            &SystemClock,
        )?;
    }
    let pending = session.resolve_approval(&actor)?;
    let turn_id = session.current_turn().cloned().ok_or_else(|| {
        CoreError::TurnRejected("approval resume without an active turn".to_owned())
    })?;
    let session_id = session.id().clone();

    // Re-enter the logical turn span after approval resolution.
    let turn_span = tracing::info_span!(
        "agent.turn",
        turn_id = %turn_id,
        session_id = %session_id,
    );
    let _turn_guard = turn_span.enter();

    let ctx = TurnInputContext {
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        metadata: serde_json::Value::Null,
    };
    let handle = TurnHandle {
        turn_id,
        session_id,
    };
    // Moduldoku „Siebter Nachtrag": wie in `resume_after_child_with_approvals`
    // — der Steuerblock des ursprünglichen Turns überlebt die Approval-Pause
    // auf der Session; nur ohne hinterlegten Block (Session nie über
    // `run_turn`/`run_turn_durable` gestartet) entsteht hier ein frischer.
    let control = session
        .active_turn_control()
        .cloned()
        .unwrap_or_else(TurnControl::new);

    // Fix A (Moduldoku „Achter Nachtrag"): jeder Fehlerausstieg dieser
    // Funktion nach erfolgreich abgeschlossenem `resolve_approval` (oben) lief
    // bisher direkt aus der Funktion heraus (`?`, `return …`), ohne
    // `transition_after_turn_failure` zu durchlaufen — die Session blieb dann
    // dauerhaft in `Running` hängen, obwohl `run_turn_with_approvals` und
    // `resume_after_child_with_approvals` dieses Muster bereits korrekt
    // anwenden. Der gesamte Auflösungspfad läuft deshalb jetzt in einem
    // inneren async-Block: `return`/`?` darin beenden nur diesen Block
    // (dieselbe Scope-Regel wie bei Closures), nicht die Funktion, sodass
    // genau ein `Result` unten geprüft und — im Fehlerfall genau einmal — die
    // Übergangsfunktion aufgerufen wird, bevor es zurückgegeben wird.
    let outcome: CoreResult<TurnOutcome> = async {
        match resolution {
            ApprovalResolution::Reject { reason } => {
                let denied_result = ToolCallResult::error(format!("denied by user: {reason}"));
                notify_tool_outcome(
                    session,
                    pending.call.name.as_str(),
                    &pending.call.arguments,
                    &denied_result,
                );
                notify_tool_progress(session);
                session
                    .history_mut()
                    .push_tool_result(pending.call.id, denied_result, 0);
                persist_last(session, store).await?;
                drive_turn(session, model, store, approvals, &ctx, handle, control).await
            }
            ApprovalResolution::Approve => {
                if let Some(role) = handoff_role(&pending.call.name) {
                    let spawner = session.registry().spawner().cloned().ok_or_else(|| {
                        CoreError::HandoffFailed {
                            role: role.clone(),
                            reason: "no AgentSpawner registered".to_owned(),
                        }
                    })?;
                    // Vor dem Verschieben der Argumente in den SpawnInput lesen.
                    let question = child_question(&pending.call.arguments);
                    let input = SpawnInput {
                        parent_session_id: session.id().clone(),
                        handoff_call_id: pending.call.id.clone(),
                        instructions: None,
                        context: pending.call.arguments,
                        // Hereditär, nie neu erfunden: dieser Handoff deklariert
                        // selbst keine eigene Kontextdecke (`call.arguments`
                        // trägt keine), also reicht dieses Feld exakt die bereits
                        // geschnittene Decke des laufenden Turns durch — dieselbe
                        // Regel, mit der `ManagedAgentSpawner::admit`
                        // (`child_controller.rs`) Sandbox und Trace im selben
                        // Schritt vererbt. `ContextCeiling::default()` stünde
                        // hier für eine erfundene Grenze, die nichts mit der
                        // tatsächlichen Autorität dieser Sitzung zu tun hätte.
                        ceiling: session
                            .spawn_context()
                            .and_then(|spawn_context| spawn_context.ceiling.clone()),
                    };
                    let context = match governed_spawn_context(session) {
                        Ok(context) => context,
                        Err(error) => {
                            let result = missing_tool_execution_context_result(error)?;
                            session
                                .history_mut()
                                .push_tool_result(pending.call.id, result, 0);
                            persist_last(session, store).await?;
                            return drive_turn(session, model, store, approvals, &ctx, handle, control)
                                .await;
                        }
                    };
                    let child = spawner
                        .spawn_child(&role, input, context.sandbox, context.suggestions)
                        .await
                        .map_err(|error| CoreError::HandoffFailed {
                            role: role.clone(),
                            reason: error.to_string(),
                        })?;
                    session.begin_handoff(child.clone(), pending.call.id.clone(), role.clone())?;
                    // Ein genehmigungspflichtiger Handoff wird ausschließlich hier
                    // gespawnt (`drive_turn` kehrt vorher mit `AwaitingApproval`
                    // zurück). Ohne dieses Event bliebe ein solches Kind für jeden
                    // Beobachter unsichtbar; doppelt gemeldet wird es nicht.
                    emit(
                        session,
                        TurnEvent::ChildSpawned {
                            turn_id: ctx.turn_id.clone(),
                            child: child.clone(),
                            role: role.clone(),
                            question,
                        },
                    );
                    Ok(TurnOutcome::AwaitingChild {
                        child,
                        call_id: pending.call.id,
                        role,
                    })
                } else {
                    let tool_name = pending.call.name.to_string();
                    let tool_span = tracing::info_span!("tool.call", tool_name = %tool_name);
                    emit(
                        session,
                        TurnEvent::ToolCallRequested {
                            turn_id: ctx.turn_id.clone(),
                            call_id: pending.call.id.clone(),
                            tool_name: tool_name.clone(),
                            arguments: pending.call.arguments.clone(),
                        },
                    );
                    // Fix B (Moduldoku „Siebter Nachtrag"): dieser Rückfrage-
                    // Fortsetzungspfad hatte bis hierher keine `was_cancelled`-
                    // Weiche — ein hier hängender Ausführer war gegen Cancel
                    // vollständig blind. Struktur jetzt identisch zum
                    // sequenziellen Pfad oben: der Aufruf selbst racet gegen
                    // `control`s `CancelToken`, und ein Treffer geht über
                    // denselben `cancel_turn_with_pending_calls`-Weg wie ein
                    // Prüfpunkt-Treffer.
                    let (result, was_cancelled): ((ToolCallResult, u64), bool) = async {
                        match find_executor(session, &pending.call.name) {
                            Some(executor) => {
                                let (result, duration_ms, cancelled) = match tool_execution_context(
                                    session,
                                    &ctx,
                                    control.cancel_token(),
                                ) {
                                    Ok(execution_context) => {
                                        let started = std::time::Instant::now();
                                        let outcome = tokio::select! {
                                            biased;
                                            () = control.cancel_token().cancelled() => Err(ToolsError::Cancelled),
                                            outcome = executor.traced_execute(&execution_context, &pending.call) => outcome,
                                        };
                                        let duration_ms = started.elapsed().as_millis() as u64;
                                        let cancelled = matches!(outcome, Err(ToolsError::Cancelled));
                                        let result = match outcome {
                                            Ok(output) => output_to_result(output),
                                            Err(e) => ToolCallResult::error(e.to_string()),
                                        };
                                        (result, duration_ms, cancelled)
                                    }
                                    Err(error) => {
                                        (missing_tool_execution_context_result(error)?, 0, false)
                                    }
                                };
                                tracing::info!(
                                    duration_ms = duration_ms,
                                    status = if result.is_success() { "ok" } else { "err" },
                                    "tool.execute",
                                );
                                Ok::<_, CoreError>(((result, duration_ms), cancelled))
                            }
                            None => {
                                tracing::info!(duration_ms = 0u64, status = "err", "tool.execute",);
                                Ok::<_, CoreError>((
                                    (
                                        ToolCallResult::error(format!(
                                            "no executor for tool '{}'",
                                            pending.call.name
                                        )),
                                        0u64,
                                    ),
                                    false,
                                ))
                            }
                        }
                    }
                    .instrument(tool_span)
                    .await?;
                    if was_cancelled {
                        // Derselbe Weg wie im sequenziellen Pfad oben: der
                        // laufende Call bekommt sein eigenes (bereits als
                        // Cancel-Fehler geformtes) Ergebnis, dann endet der Turn
                        // über `cancel_turn_with_pending_calls`. Ein
                        // Rückfrage-Resume setzt genau einen Call fort — es gibt
                        // keine Geschwister-Calls derselben Modellantwort, die
                        // hier noch anstünden (die pausierende `AskUser`-Antwort
                        // verließ die sequenzielle Schleife oben sofort mit
                        // `AwaitingApproval`, ohne die übrigen Calls der Antwort
                        // überhaupt in den Verlauf zu schreiben — siehe
                        // `ApprovalDecision::AskUser` im sequenziellen Pfad).
                        // `remaining: Vec::new()` ist deshalb korrekt, nicht nur
                        // bequem.
                        let (result_value, result_duration_ms) = result;
                        emit(
                            session,
                            TurnEvent::ToolCallCompleted {
                                turn_id: ctx.turn_id.clone(),
                                call_id: pending.call.id.clone(),
                                result: result_value.clone(),
                                duration_ms: result_duration_ms,
                            },
                        );
                        notify_tool_outcome(
                            session,
                            &tool_name,
                            &pending.call.arguments,
                            &result_value,
                        );
                        notify_tool_progress(session);
                        session
                            .history_mut()
                            .push_tool_result(pending.call.id, result_value, result_duration_ms);
                        persist_last(session, store).await?;
                        return cancel_turn_with_pending_calls(
                            session,
                            store,
                            handle,
                            harw_types::TokenUsage::default(),
                            control.cancel_token().reason().unwrap_or(CancelReason::User),
                            Vec::new(),
                        )
                        .await;
                    }
                    let (result_value, result_duration_ms) = result;
                    emit(
                        session,
                        TurnEvent::ToolCallCompleted {
                            turn_id: ctx.turn_id.clone(),
                            call_id: pending.call.id.clone(),
                            result: result_value.clone(),
                            duration_ms: result_duration_ms,
                        },
                    );
                    notify_tool_outcome(session, &tool_name, &pending.call.arguments, &result_value);
                    notify_tool_progress(session);
                    session
                        .history_mut()
                        .push_tool_result(pending.call.id, result_value, result_duration_ms);
                    persist_last(session, store).await?;
                    drive_turn(session, model, store, approvals, &ctx, handle, control).await
                }
            }
        }
    }
    .await;

    if let Err(error) = &outcome {
        transition_after_turn_failure(session, &ctx, error);
    }
    outcome
}

/// Der Model-/Tool-Zyklus. Setzt `Running`-State voraus.
///
/// Emits the `model.request`, `model.response`, `tool.call`, `tool.execute`,
/// and `approval.wait` tracing events and spans described in the module doc.
/// Must be called while an `agent.turn` span is already entered by the caller.
async fn drive_turn(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: Option<&ApprovalStore>,
    ctx: &TurnInputContext,
    handle: TurnHandle,
    control: TurnControl,
) -> CoreResult<TurnOutcome> {
    // Ein Provider kann trotz expliziter Fortsetzungsanweisung erneut am
    // Ausgabelimit enden. Die feste Obergrenze verhindert einen stillen,
    // kostenpflichtigen Endlos-Loop; danach erhält der Aufrufer weiterhin den
    // ehrlichen `Truncated`-Ausgang mitsamt bereits persistiertem Teiltext.
    const MAX_AUTOMATIC_CONTINUATIONS: u32 = 3;
    const CONTINUATION_INSTRUCTION: &str = "Die unmittelbar vorherige Assistant-Antwort wurde wegen eines Ausgabelimits abgeschnitten. Setze exakt an ihrer letzten Stelle fort, ohne Text zu wiederholen oder neu anzufangen. Schließe den ursprünglichen Auftrag eigenständig ab.";

    let mut total_usage = harw_types::TokenUsage::default();
    let mut automatic_continuations = 0u32;
    // Fortlaufende Modell-Runden-Nummer dieses `drive_turn`-Aufrufs, für
    // `UsageRound::round` — bei jedem Modellaufruf inkrementiert, bevor die
    // Runde persistiert wird.
    let mut round: u32 = 0;
    // Nutzung der zuletzt abgeschlossenen Modell-Runde — für `maybe_compact`
    // an beiden Call-Sites (innerhalb der Schleife und nach Turn-Ende, wo
    // `response` bereits außer Scope ist). Kein Default-Vorbelegungswert:
    // jeder Lesezugriff (Zeile ~2536, nach der Schleife) liegt hinter der
    // ersten Zuweisung unten (nach dem ersten Modellaufruf dieser Runde);
    // ein Default hier wäre vor jedem Lesezugriff unbedingt überschrieben
    // und damit ein toter Store.
    let mut last_round_usage: harw_types::TokenUsage;
    // Wanduhr-Nullpunkt dieses Aufrufs (siehe Moduldoku „Fünfter Nachtrag").
    // Wiederholte Aufrufe (weiterer Schleifendurchlauf) sind ein No-op — nur
    // der erste zählt.
    control.start();

    // Turn-Wächter (Addendum F+G): nur erzeugt, wenn die Session-Policy sie
    // einschaltet — `TurnGuard` lebt ausschließlich für die Dauer dieses
    // `drive_turn`-Aufrufs (siehe Moduldoku „Fünfter Nachtrag" zur analogen
    // Lücke bei `TurnControl`: ein pausierter und über `resume_*`
    // fortgesetzter Turn bekommt hier ebenfalls einen frischen Wächter ohne
    // Turn-übergreifenden Zustand).
    let guard_policy = session.guard_policy();
    let mut guard: Option<TurnGuard> = guard_policy
        .enabled
        .then(|| TurnGuard::new(guard_policy, session.id()));
    // Signaturen (Tool-Name + kanonische Argumente) erfolgreicher Aufrufe,
    // bereits in einer früheren Runde dieses Turns gesehen — für die
    // Fortschritts-Erkennung „mindestens ein erfolgreicher Tool-Aufruf mit in
    // diesem Turn neuer Signatur" (Vertrag `TurnGuard::observe_round_end`).
    let mut turn_seen_success_signatures: HashSet<String> = HashSet::new();
    // Hinweistext eines `Warn`-Befunds, der keinem eigenen Tool-Ergebnis
    // dieser Runde mehr zugeordnet werden konnte (z. B. ein
    // Runden-Ende-Befund nach der letzten Werkzeugausführung der Runde) —
    // wird dem nächsten Tool-Ergebnis des Turns vorangestellt, sobald eines
    // entsteht.
    let mut pending_guard_hint: Option<String> = None;

    // Einmal je Turn, vor dem ersten Model-Aufruf: das Kassenbuch des
    // `context.load`-Ausführers vorbelegen, sofern die Sitzung ein
    // `ContextProgram` deklariert (siehe `seed_context_load_ledger`-Doku).
    // Bewusst außerhalb der Schleife unten — `ContextLoadExecutor::seed_turn`
    // überschreibt das Kassenbuch eines Turns vollständig (auch
    // `loads_used`), ein Aufruf je Model-Runde würde also innerhalb desselben
    // Turns bereits verbuchte `context.load`-Aufrufe verwerfen.
    seed_context_load_ledger(session, &ctx.turn_id);

    loop {
        // Prüfpunkt vor jedem Modellaufruf (Moduldoku „W4a A-LOOP"): Abbruch,
        // Modellrunden, Ausgabe-Tokens, Wanduhr. Vor der Context-/Instructions-
        // Montage, damit ein bereits erschöpftes Budget diese Arbeit nicht
        // mehr verursacht.
        if let Some(reason) = control.model_checkpoint() {
            return cancel_turn(session, handle, total_usage, reason).await;
        }

        // 1./2. Context + Instructions.
        let mut fragments = gather_context(session, ctx).await;
        if automatic_continuations > 0 {
            match continuation_fragment(CONTINUATION_INSTRUCTION) {
                Some(fragment) => fragments.push(fragment),
                None => {
                    tracing::warn!("turn_loop.continuation_fragment_unavailable");
                }
            }
        }
        let instructions = load_instructions(session).await;
        let tools = collect_tools(session)?;

        // Nachtrag F (Delegationsprojektion): EIN deterministischer
        // Kontextblock, NACH den Tools angehängt (stabiler Teil — die Liste
        // ändert sich selten, sortiert vom Spawner geliefert). Leer ⇒ nichts.
        if let Some(spawner) = session.registry().spawner() {
            let delegation_targets = spawner.delegation_target_names(session.id());
            if let Some(fragment) = delegation_targets_fragment(&delegation_targets) {
                fragments.push(fragment);
            }
        }

        // Programm- und Decken-bewusste Montage (siehe
        // `ModelRequest::with_context_program`s Moduldoku, Abschnitt „Zwei
        // Wege zur Kontextmontage"): läuft nur, wenn die Sitzung **beide**
        // deklariert; sonst fällt sie intern auf denselben Byte-Budget-Pfad
        // zurück, den diese Datei vor diesem Knoten direkt aufgerufen hat —
        // eine Sitzung ohne `ContextProgram` sieht dadurch keine Änderung.
        let program = session.context_program();
        let ceiling = session
            .spawn_context()
            .and_then(|spawn_context| spawn_context.ceiling.as_ref());

        // 4. Model-Call (provider-neutral).
        let request = ModelRequest::with_context_program(
            instructions,
            fragments,
            session.history().clone(),
            tools,
            session.context_budget(),
            program,
            ceiling,
        )?
        .with_reasoning_effort(session.reasoning_effort())
        .with_model_id(session.active_model().cloned())
        .with_provider_id(session.active_provider().cloned())
        .with_tool_result_max_bytes(control.limits().tool_result_max_bytes_hint())
        .with_cancel_token(control.cancel_token().clone())
        .with_identity(request_identity(session));
        let request = match stream_sink_for_round(session, &handle.turn_id, &total_usage) {
            Some(sink) => request.with_stream_sink(sink),
            None => request,
        };
        let history_items_dropped = request.context_assembly.history_items_dropped;

        // Emit model.request event: byte-count proxy via system_prompt +
        // instruction fragments length (ModelRequest is not serde::Serialize).
        let request_size_bytes: usize = request.system_prompt.len()
            + request
                .instruction_fragments
                .iter()
                .map(|s| s.len())
                .sum::<usize>();
        tracing::info!(size_bytes = request_size_bytes, "model.request");

        // Races the model call itself against `control`'s `CancelToken`
        // (W4a A-LOOP Moduldoku: "Der Modellaufruf selbst läuft gegen
        // `CancelToken::cancelled`"). `biased` ensures an already-cancelled
        // token wins even if the model future also happens to be ready —
        // matching `model_checkpoint()`'s own cancel-first precedence.
        // `request.cancel` (`with_cancel_token` above) lets a
        // cancel-aware provider (e.g. `RetryingProvider::race_respond`)
        // abort its own retry loop early; this `select!` is the backstop
        // for every provider, cancel-aware or not.
        let response = tokio::select! {
            biased;
            _ = control.cancel_token().cancelled() => {
                return cancel_turn(
                    session,
                    handle,
                    total_usage,
                    control.cancel_token().reason().unwrap_or(CancelReason::User),
                )
                .await;
            }
            result = model.respond(request) => result,
        };
        // `ModelError::Cancelled` can also surface from inside `respond()`
        // itself (a cancel-aware provider observed the token before this
        // `select!` did) — routed through the identical `cancel_turn` path
        // rather than the ordinary `?`-propagated `CoreError::Model(...)`.
        let response = match response {
            Ok(response) => response,
            Err(crate::model::ModelError::Cancelled) => {
                return cancel_turn(
                    session,
                    handle,
                    total_usage,
                    control
                        .cancel_token()
                        .reason()
                        .unwrap_or(CancelReason::User),
                )
                .await;
            }
            Err(error) => return Err(error.into()),
        };
        control.record_model_round();
        control.record_usage(&response.usage);
        notify_round_progress(session, &response.usage);
        total_usage.add(&response.usage);
        round += 1;
        last_round_usage = response.usage.clone();
        // Live-Stand nach abgeschlossener Runde (auch der Pro-Runde-Fallback
        // nicht streamender Provider landet hier).
        emit(
            session,
            TurnEvent::UsageUpdated {
                turn_id: handle.turn_id.clone(),
                round: response.usage.clone(),
                turn_total: total_usage.clone(),
                final_round: true,
            },
        );
        emit(
            session,
            TurnEvent::ContextUpdated {
                turn_id: handle.turn_id.clone(),
                used_tokens: response
                    .usage
                    .prompt_tokens()
                    .saturating_add(response.usage.output_tokens),
                window_tokens: session.auto_compact().map_or(
                    0,
                    crate::auto_compact::AutoCompactPolicy::context_window_tokens,
                ),
                history_items_dropped: u32::try_from(history_items_dropped).unwrap_or(u32::MAX),
            },
        );

        // Nutzung dieser Runde persistieren (best effort — ein Store-Fehler
        // darf den Turn niemals scheitern lassen, siehe Vertrag
        // `StateStore::record_usage`).
        let usage_round = UsageRound {
            round,
            provider_id: session.active_provider().map(|p| p.as_str().to_owned()),
            model_id: session.active_model().map(|m| m.as_str().to_owned()),
            usage: response.usage.clone(),
            cache_strategy: None,
        };
        if let Err(error) = store.record_usage(session.id(), &usage_round).await {
            tracing::warn!(%error, "turn_loop.record_usage_failed");
        }

        // Emit model.response event: size_bytes from tool_calls JSON +
        // optional message length; tool_call_count for scheduling insight.
        let response_size_bytes: usize = response.message.as_deref().map_or(0, |m| m.len())
            + serde_json::to_vec(&response.tool_calls)
                .map(|v| v.len())
                .unwrap_or(0);
        tracing::info!(
            size_bytes = response_size_bytes,
            tool_call_count = response.tool_calls.len(),
            "model.response",
        );

        // Für die Fortschritts-Erkennung des reinen Text-Zweigs unten
        // („Assistant-Text ohne Tool-Aufrufe") vor dem Move gesichert.
        let response_had_text = response.message.is_some();
        let response_had_no_tool_calls = response.tool_calls.is_empty();
        let response_stop = response.stop.clone();
        // Fortschritt durch erfolgreiche Tool-Aufrufe mit neuer Signatur wird
        // unten im Tool-Call-Loop gesetzt.
        let mut round_progressed_by_tools = false;

        // Reasoning sichtbar machen (Welle 3 — 3e): nur für die UIA-Root-
        // Session, und nur, wenn sich lesbarer `"thinking"`-Text extrahieren
        // ließ (redacted/verschlüsseltes Reasoning bleibt unsichtbar). VOR
        // der AssistantMessage eingefügt — Denken kommt vor der Antwort.
        // `to_model_messages` (history.rs) überspringt `TurnItem::Reasoning`
        // beim nächsten Provider-Request explizit, das Item stört dort also
        // nicht.
        if is_uia_root_session(session) {
            if let Some(text) = response.reasoning.as_ref().and_then(extract_thinking_text) {
                let reasoning_id = harw_types::ItemId::new();
                session
                    .history_mut()
                    .push(TurnItem::Reasoning(ReasoningItem {
                        id: reasoning_id.clone(),
                        summary_text: vec![text.clone()],
                        raw_content: Vec::new(),
                    }));
                persist_last(session, store).await?;
                emit(
                    session,
                    TurnEvent::ItemAdded {
                        turn_id: handle.turn_id.clone(),
                        item: TurnItem::Reasoning(ReasoningItem {
                            id: reasoning_id,
                            summary_text: vec![text],
                            raw_content: Vec::new(),
                        }),
                    },
                );
            }
        }

        if let Some(text) = response.message {
            let phase = if response.tool_calls.is_empty() {
                Some(harw_types::MessagePhase::FinalAnswer)
            } else {
                Some(harw_types::MessagePhase::Commentary)
            };
            let text_for_event = text.clone();
            session.history_mut().push_assistant_text(text, phase);
            persist_last(session, store).await?;
            emit(
                session,
                TurnEvent::ItemAdded {
                    turn_id: handle.turn_id.clone(),
                    item: TurnItem::AssistantMessage(AssistantMessageItem {
                        id: harw_types::ItemId::new(),
                        content: vec![ContentPart::Text {
                            text: text_for_event,
                        }],
                        phase,
                    }),
                },
            );
        }

        // Ein Provider darf nie dazu verleitet werden, abgeschnittene oder
        // gefilterte Tool-Argumente auszuführen. Die Antwort ist trotzdem
        // bereits als Text erhalten, damit ein Nutzer den sichtbaren Teil
        // nicht verliert.
        if !response_had_no_tool_calls {
            match response_stop {
                crate::model::StopReason::MaxTokens
                | crate::model::StopReason::ContextWindowExceeded => {
                    return finish_model_stop(
                        session,
                        handle,
                        total_usage,
                        pending_guard_hint.clone(),
                        TurnOutcome::Truncated,
                    )
                    .await;
                }
                crate::model::StopReason::Refusal { detail } => {
                    return finish_model_stop(
                        session,
                        handle,
                        total_usage,
                        pending_guard_hint.clone(),
                        TurnOutcome::Refused { detail },
                    )
                    .await;
                }
                crate::model::StopReason::ContentFilter => {
                    return finish_model_stop(
                        session,
                        handle,
                        total_usage,
                        pending_guard_hint.clone(),
                        TurnOutcome::Refused { detail: None },
                    )
                    .await;
                }
                _ => {}
            }
        }

        // 6. Keine Tool-Calls mehr ⇒ Turn fertig.
        if response_had_no_tool_calls {
            // Runden-Ende-Beobachtung (Addendum F+G): diese Runde bestand nur
            // aus Assistant-Text — Fortschritt genau dann, wenn tatsächlich
            // Text kam (Vertrag: „Assistant-Text ohne Tool-Aufrufe").
            if let Some(g) = guard.as_mut() {
                match g.observe_round_end(response_had_text) {
                    GuardVerdict::Continue => {}
                    GuardVerdict::Warn { event, hint } => {
                        report_drift(session, store, &event).await;
                        // Keine Werkzeugausführung mehr in dieser Runde, an
                        // die der Hinweis sofort angehängt werden könnte —
                        // er wird dem nächsten Tool-Ergebnis des Turns
                        // vorangestellt, falls noch eines entsteht.
                        pending_guard_hint = Some(match pending_guard_hint.take() {
                            Some(existing) => format!("{existing}\n\n{hint}"),
                            None => hint,
                        });
                    }
                    GuardVerdict::Abort { event, hint } => {
                        report_drift(session, store, &event).await;
                        tracing::warn!(hint = %hint, "turn_loop.guard_abort");
                        return cancel_turn(session, handle, total_usage, CancelReason::Budget)
                            .await;
                    }
                }
            }
            match response_stop {
                crate::model::StopReason::MaxTokens
                    if automatic_continuations < MAX_AUTOMATIC_CONTINUATIONS =>
                {
                    automatic_continuations += 1;
                    tracing::info!(
                        continuation = automatic_continuations,
                        "turn_loop.auto_continue_after_max_tokens"
                    );
                    maybe_compact(session, model, store, &last_round_usage, false).await;
                    continue;
                }
                crate::model::StopReason::MaxTokens
                | crate::model::StopReason::ContextWindowExceeded => {
                    return finish_model_stop(
                        session,
                        handle,
                        total_usage,
                        pending_guard_hint.clone(),
                        TurnOutcome::Truncated,
                    )
                    .await;
                }
                crate::model::StopReason::Refusal { detail } => {
                    return finish_model_stop(
                        session,
                        handle,
                        total_usage,
                        pending_guard_hint.clone(),
                        TurnOutcome::Refused { detail },
                    )
                    .await;
                }
                crate::model::StopReason::ContentFilter => {
                    return finish_model_stop(
                        session,
                        handle,
                        total_usage,
                        pending_guard_hint.clone(),
                        TurnOutcome::Refused { detail: None },
                    )
                    .await;
                }
                _ => break,
            }
        }

        // Prüfpunkt vor jeder Werkzeugausführung: Abbruch, Aufrufzahl, Wanduhr.
        // Trifft er zu, bekommt jeder noch offene Tool-Call dieser Antwort ein
        // synthetisches Fehlerergebnis, damit der Verlauf provider-gültig
        // bleibt (jeder `tool_call` hat sein `tool_result`) — siehe Moduldoku
        // „W4a A-LOOP".
        if let Some(reason) = control.tool_checkpoint(response.tool_calls.len()) {
            return cancel_turn_with_pending_calls(
                session,
                store,
                handle,
                total_usage,
                reason,
                response.tool_calls,
            )
            .await;
        }
        control.record_tool_calls(response.tool_calls.len());

        // Die Vorprüfung des Parallel-Pfads kann Guardrail-Entscheidungen
        // bereits eingeholt haben. Sie leben genau eine Modellantwort lang und
        // werden unten verbraucht, damit kein Handler doppelt gefragt wird.
        let mut prepared = PreparedApprovals::default();
        match try_execute_parallel_calls(
            session,
            store,
            ctx,
            &response.tool_calls,
            &mut prepared,
            guard.as_mut(),
            &mut turn_seen_success_signatures,
            &mut pending_guard_hint,
            &mut round_progressed_by_tools,
            &control,
        )
        .await?
        {
            ParallelOutcome::NotApplicable => {}
            ParallelOutcome::Executed => continue,
            ParallelOutcome::Aborted(reason) => {
                return cancel_turn(session, handle, total_usage, reason).await;
            }
        }

        // 5. Tool-Call-Loop.
        // `while let` statt `for`: bei einem `TurnGuard`-`Abort` mitten in
        // dieser Runde (Addendum F+G) müssen die noch nicht ausgeführten
        // Calls — der restliche Iterator-Inhalt — an
        // `cancel_turn_with_pending_calls` gehen, sonst bliebe ein
        // `tool_call` ohne `tool_result` im Verlauf zurück (siehe dessen
        // Doku).
        let mut tool_call_iter = response.tool_calls.into_iter().enumerate();
        while let Some((position, call)) = tool_call_iter.next() {
            session.history_mut().push_tool_call(
                call.id.clone(),
                call.name.to_string(),
                call.arguments.clone(),
            );
            persist_last(session, store).await?;
            emit(
                session,
                TurnEvent::ToolCallRequested {
                    turn_id: handle.turn_id.clone(),
                    call_id: call.id.clone(),
                    tool_name: call.name.to_string(),
                    arguments: call.arguments.clone(),
                },
            );

            // a. Guardrail. Eine Entscheidung aus der Parallel-Vorprüfung wird
            // verbraucht statt neu eingeholt: ein Approval-Handler darf zu
            // demselben Call nicht zweimal befragt werden.
            let decision = match prepared.take(position) {
                Some(decision) => decision,
                None => check_approval(session, &call).await,
            };
            match decision {
                ApprovalDecision::Allow => {}
                ApprovalDecision::Deny(reason) => {
                    let mut denied_result = ToolCallResult::error(format!("denied: {reason}"));
                    let abort_reason = apply_tool_guard(
                        session,
                        store,
                        guard.as_mut(),
                        &mut turn_seen_success_signatures,
                        &mut pending_guard_hint,
                        &mut round_progressed_by_tools,
                        call.name.as_str(),
                        &call.arguments,
                        &mut denied_result,
                    )
                    .await;
                    notify_tool_outcome(
                        session,
                        call.name.as_str(),
                        &call.arguments,
                        &denied_result,
                    );
                    notify_tool_progress(session);
                    session
                        .history_mut()
                        .push_tool_result(call.id.clone(), denied_result, 0);
                    persist_last(session, store).await?;
                    // Diesem Call ist bereits ein Tool-Result gepaart — nur
                    // die noch unangetasteten restlichen Calls des
                    // Iterators brauchen ein synthetisches Ergebnis.
                    if let Some(abort_reason) = abort_reason {
                        let remaining: Vec<ToolCall> =
                            tool_call_iter.map(|(_, call)| call).collect();
                        return cancel_turn_with_pending_calls(
                            session,
                            store,
                            handle,
                            total_usage,
                            abort_reason,
                            remaining,
                        )
                        .await;
                    }
                    continue;
                }
                ApprovalDecision::AskUser(request) => {
                    // Turn pausiert bis zur Nutzer-Entscheidung.
                    let actor = session
                        .spawn_context()
                        .and_then(|context| context.approval_actor.clone())
                        .ok_or_else(|| CoreError::MissingApprovalActor {
                            session_id: session.id().to_string(),
                        })?;

                    // approval.wait span: records the pause point before the
                    // turn suspends; kind is the Display form of the request ID.
                    let approval_span = tracing::info_span!(
                        "approval.wait",
                        approval_kind = %request,
                    );
                    let _approval_guard = approval_span.enter();

                    let paused_at = jiff::Timestamp::now();
                    if let Some(approvals) = approvals {
                        approvals.issue(&ApprovalRecord {
                            request: request.clone(),
                            session: session.id().clone(),
                            call_id: call.id.clone(),
                            actor: actor.clone(),
                            issued_at: paused_at,
                        })?;
                    }
                    session.begin_approval(call.clone(), request.clone(), actor, paused_at)?;
                    return Ok(TurnOutcome::AwaitingApproval {
                        call_id: call.id,
                        request,
                    });
                }
            }

            // b. Handoff-Detection.
            if let Some(role) = handoff_role(&call.name) {
                let spawner = session.registry().spawner().cloned().ok_or_else(|| {
                    CoreError::HandoffFailed {
                        role: role.clone(),
                        reason: "no AgentSpawner registered".to_owned(),
                    }
                })?;
                let spawn_input = SpawnInput {
                    parent_session_id: session.id().clone(),
                    handoff_call_id: call.id.clone(),
                    instructions: None,
                    context: call.arguments.clone(),
                    // Siehe die Begründung an der Schwester-Konstruktionsstelle
                    // in `resume_after_approval_with_store`: hereditär, nie
                    // neu erfunden.
                    ceiling: session
                        .spawn_context()
                        .and_then(|spawn_context| spawn_context.ceiling.clone()),
                };
                let context = match governed_spawn_context(session) {
                    Ok(context) => context,
                    Err(error) => {
                        let mut result = missing_tool_execution_context_result(error)?;
                        let abort_reason = apply_tool_guard(
                            session,
                            store,
                            guard.as_mut(),
                            &mut turn_seen_success_signatures,
                            &mut pending_guard_hint,
                            &mut round_progressed_by_tools,
                            call.name.as_str(),
                            &call.arguments,
                            &mut result,
                        )
                        .await;
                        notify_tool_outcome(session, call.name.as_str(), &call.arguments, &result);
                        notify_tool_progress(session);
                        session.history_mut().push_tool_result(call.id, result, 0);
                        persist_last(session, store).await?;
                        if let Some(abort_reason) = abort_reason {
                            let remaining: Vec<ToolCall> =
                                tool_call_iter.map(|(_, call)| call).collect();
                            return cancel_turn_with_pending_calls(
                                session,
                                store,
                                handle,
                                total_usage,
                                abort_reason,
                                remaining,
                            )
                            .await;
                        }
                        continue;
                    }
                };
                let spawned = spawner
                    .spawn_child(&role, spawn_input, context.sandbox, context.suggestions)
                    .await;
                let child = spawned.map_err(|e| CoreError::HandoffFailed {
                    role: role.clone(),
                    reason: e.to_string(),
                })?;
                // state ⇒ WaitingForChild; Loop pausiert.
                session.begin_handoff(child.clone(), call.id.clone(), role.clone())?;
                emit(
                    session,
                    TurnEvent::ChildSpawned {
                        turn_id: handle.turn_id.clone(),
                        child: child.clone(),
                        role: role.clone(),
                        question: child_question(&call.arguments),
                    },
                );
                return Ok(TurnOutcome::AwaitingChild {
                    child,
                    call_id: call.id,
                    role,
                });
            }

            // c. Normale Tool-Ausführung — instrumented with tool.call span.
            let tool_name = call.name.to_string();
            // Wächter-Beratung vor der Ausführung (Addendum F+G): ein
            // Treffer wird sofort gemeldet, der Hinweis aber erst an das
            // Ergebnis DIESES Aufrufs angehängt, sobald es feststeht.
            if let Some(hint) =
                apply_pitfall_advice(session, store, &tool_name, &call.arguments).await
            {
                pending_guard_hint = Some(match pending_guard_hint.take() {
                    Some(existing) => format!("{existing}\n\n{hint}"),
                    None => hint,
                });
            }
            let tool_span = tracing::info_span!("tool.call", tool_name = %tool_name);
            // Zweites Feld: `true`, wenn der Ausführer `ToolsError::Cancelled`
            // meldete (der `ToolExecutionContext` unten trägt `control`s
            // `CancelToken`, siehe `tool_execution_context`) — ausgewertet
            // NACH `.instrument(tool_span).await?`, um denselben Weg wie ein
            // `tool_checkpoint()`-Treffer zu nehmen statt das Ergebnis als
            // normalen Tool-Fehler ans Modell zurückzuspielen.
            let (result, was_cancelled): ((ToolCallResult, u64), bool) = async {
                match find_executor(session, &call.name) {
                    Some(executor) => {
                        let (result, duration_ms, cancelled) = match tool_execution_context(
                            session,
                            ctx,
                            control.cancel_token(),
                        ) {
                            Ok(execution_context) => {
                                let started = std::time::Instant::now();
                                // Fix B (Moduldoku „Siebter Nachtrag"): racet
                                // den Aufruf selbst gegen `control`s
                                // `CancelToken`, exakt wie der Modellaufruf-Race
                                // oben — ein Ausführer, der `ctx.cancel()`
                                // selbst nie abfragt, blockiert den Turn damit
                                // nicht mehr bis zu seinem eigenen Ende.
                                let outcome = tokio::select! {
                                    biased;
                                    () = control.cancel_token().cancelled() => Err(ToolsError::Cancelled),
                                    outcome = executor.traced_execute(&execution_context, &call) => outcome,
                                };
                                let duration_ms = started.elapsed().as_millis() as u64;
                                let cancelled = matches!(outcome, Err(ToolsError::Cancelled));
                                let result = match outcome {
                                    Ok(output) => output_to_result(output),
                                    Err(e) => ToolCallResult::error(e.to_string()),
                                };
                                (result, duration_ms, cancelled)
                            }
                            Err(error) => (missing_tool_execution_context_result(error)?, 0, false),
                        };
                        tracing::info!(
                            duration_ms = duration_ms,
                            status = if result.is_success() { "ok" } else { "err" },
                            "tool.execute",
                        );
                        Ok::<_, CoreError>(((result, duration_ms), cancelled))
                    }
                    None => {
                        tracing::info!(duration_ms = 0u64, status = "err", "tool.execute",);
                        Ok::<_, CoreError>((
                            (
                                ToolCallResult::error(format!(
                                    "no executor for tool '{}'",
                                    call.name
                                )),
                                0u64,
                            ),
                            false,
                        ))
                    }
                }
            }
            .instrument(tool_span)
            .await?;
            if was_cancelled {
                // Derselbe Weg wie ein Vorab-`tool_checkpoint()`-Treffer
                // (Moduldoku „W4a A-LOOP"): der laufende Call bekommt sein
                // eigenes (bereits als Cancel-Fehler geformtes) Ergebnis —
                // sein `tool_call`-Eintrag steht schon im Verlauf (oben, vor
                // dieser Runde) — und alle noch nicht gestarteten Calls
                // derselben Antwort gehen über `cancel_turn_with_pending_calls`
                // denselben Weg wie die übrigen Prüfpunkt-Treffer dieser
                // Schleife.
                let (result_value, result_duration_ms) = result;
                emit(
                    session,
                    TurnEvent::ToolCallCompleted {
                        turn_id: handle.turn_id.clone(),
                        call_id: call.id.clone(),
                        result: result_value.clone(),
                        duration_ms: result_duration_ms,
                    },
                );
                notify_tool_outcome(session, &tool_name, &call.arguments, &result_value);
                notify_tool_progress(session);
                session
                    .history_mut()
                    .push_tool_result(call.id, result_value, result_duration_ms);
                persist_last(session, store).await?;
                let remaining: Vec<ToolCall> = tool_call_iter.map(|(_, call)| call).collect();
                return cancel_turn_with_pending_calls(
                    session,
                    store,
                    handle,
                    total_usage,
                    control
                        .cancel_token()
                        .reason()
                        .unwrap_or(CancelReason::User),
                    remaining,
                )
                .await;
            }
            let (mut result_value, result_duration_ms) = result;
            let abort_reason = apply_tool_guard(
                session,
                store,
                guard.as_mut(),
                &mut turn_seen_success_signatures,
                &mut pending_guard_hint,
                &mut round_progressed_by_tools,
                &tool_name,
                &call.arguments,
                &mut result_value,
            )
            .await;
            let call_id_for_event = call.id.clone();
            emit(
                session,
                TurnEvent::ToolCallCompleted {
                    turn_id: handle.turn_id.clone(),
                    call_id: call_id_for_event,
                    result: result_value.clone(),
                    duration_ms: result_duration_ms,
                },
            );
            // d. ToolResult in History.
            notify_tool_outcome(session, &tool_name, &call.arguments, &result_value);
            notify_tool_progress(session);
            session
                .history_mut()
                .push_tool_result(call.id, result_value, result_duration_ms);
            persist_last(session, store).await?;
            if let Some(abort_reason) = abort_reason {
                let remaining: Vec<ToolCall> = tool_call_iter.map(|(_, call)| call).collect();
                return cancel_turn_with_pending_calls(
                    session,
                    store,
                    handle,
                    total_usage,
                    abort_reason,
                    remaining,
                )
                .await;
            }
        }

        // Runden-Ende-Beobachtung (Addendum F+G): alle Tool-Ergebnisse dieser
        // Runde stehen bereits fest (sequenzieller Pfad; der Parallel-Pfad
        // `try_execute_parallel_calls` springt per `continue` direkt zurück
        // zu Schritt 4 und überspringt diese Stelle ebenso wie die
        // Auto-Compaction unten — dieselbe, bereits bestehende Lücke).
        if let Some(g) = guard.as_mut() {
            match g.observe_round_end(round_progressed_by_tools) {
                GuardVerdict::Continue => {}
                GuardVerdict::Warn { event, hint } => {
                    report_drift(session, store, &event).await;
                    pending_guard_hint = Some(match pending_guard_hint.take() {
                        Some(existing) => format!("{existing}\n\n{hint}"),
                        None => hint,
                    });
                }
                GuardVerdict::Abort { event, hint } => {
                    report_drift(session, store, &event).await;
                    tracing::warn!(hint = %hint, "turn_loop.guard_abort");
                    return cancel_turn(session, handle, total_usage, CancelReason::Budget).await;
                }
            }
        }

        // Auto-Compaction an einer sicheren Grenze: alle Tool-Ergebnisse
        // dieser Runde sind bereits in der Historie, der nächste Model-
        // Request ist aber noch nicht gebaut — hier mutiert `compact_session`
        // die Historie also nie eine in-flight-Anfrage an. Höchstens einmal
        // je Runde (dieser Call-Site läuft genau einmal pro Schleifendurchlauf).
        //
        // `task_completed` wird vor dem Aufruf in eine eigene Variable
        // gelegt: `maybe_compact` nimmt `session` mutable entgegen, ein
        // verschachtelter `turn_completed_a_plan_step(session)`-Aufruf als
        // Argumentausdruck würde eine gleichzeitige unveränderliche Ausleihe
        // gegen die bereits laufende veränderliche Ausleihe erzeugen.
        let task_completed = turn_completed_a_plan_step(session);
        maybe_compact(session, model, store, &last_round_usage, task_completed).await;

        // Zurück zu Schritt 4 (nächster Model-Call).
    }

    // Welle FANIN-K: ein Wächter-Hinweis, der am Ende der letzten Runde
    // entstand (`response_had_no_tool_calls`-Zweig oben), aber keinem
    // weiteren Tool-Ergebnis mehr zugeordnet werden konnte, weil die Runde
    // ohne Tool-Aufrufe endete, ging bislang spurlos verloren — er wird
    // stattdessen an das letzte Item der Historie angehängt.
    if let Some(hint) = pending_guard_hint.take() {
        session.history_mut().append_hint_to_last(&hint);
    }

    // 7. Observer + Turn-Ende.
    emit(
        session,
        TurnEvent::TurnCompleted {
            turn_id: handle.turn_id.clone(),
            usage: Some(total_usage.clone()),
        },
    );
    notify_turn_stop(
        session,
        &TurnStopInput {
            session_id: handle.session_id.clone(),
            turn_id: handle.turn_id.clone(),
            token_usage: total_usage.clone(),
        },
    );
    // Auto-Compaction am Ende eines erfolgreich abgeschlossenen Turns — die
    // zweite sichere Grenze neben der innerhalb der Schleife oben: kein
    // in-flight-Request existiert mehr, der nächste Turn beginnt erst mit
    // dem nächsten `run_turn`-Aufruf. `task_completed` wird wie oben vor dem
    // Aufruf ausgewertet (siehe Kommentar an der ersten Call-Site).
    let task_completed = turn_completed_a_plan_step(session);
    maybe_compact(session, model, store, &last_round_usage, task_completed).await;

    // Projektgedächtnis: alle Tool-Ergebnisse dieses Turns wurden bereits
    // über `notify_tool_outcome` gemeldet; hier, am erfolgreichen Turn-Ende,
    // erfährt der Beobachter, dass die Runde abgeschlossen ist.
    if let Some(observer) = session.tool_outcome_observer().cloned() {
        observer.on_turn_finished(session.id());
    }

    // `drive_turn` liefert `CoreResult<TurnOutcome>` — anders als
    // `transition_after_turn_failure` (das nichts zurückgeben kann und deshalb
    // protokolliert) kann dieser Aufrufer den Fehler ehrlich weiterreichen:
    // ein Turn, der sich nicht nach `Idle` zurückschreiben lässt, ist kein
    // `Completed`. Der `?`-Aufrufer (`run_turn_with_approvals` &co.) fängt den
    // Fehler bereits über `transition_after_turn_failure` ab.
    session.complete_turn(handle, total_usage)?;
    Ok(TurnOutcome::Completed)
}

/// Prüft die [`crate::auto_compact::AutoCompactPolicy`] der Session und
/// verdichtet bei Bedarf die Historie an einer sicheren Grenze.
///
/// # Description
/// No-op, wenn die Session keine Policy gesetzt hat
/// ([`AgentSession::auto_compact`] liefert `None` — der Default für jede
/// Session, die sich nicht explizit für Auto-Compaction entscheidet).
///
/// `tokens_used` ist [`harw_types::TokenUsage::prompt_tokens`]: bei
/// OpenAI-Semantik (`cached_tokens` ⊆ `input_tokens`) genau `input_tokens`,
/// bei Anthropic (`cache_separate`) die Summe aus ungecachtem Input,
/// Cache-Read und Cache-Write — sonst würde ein fast vollständig gecachter
/// Prompt die Schwelle nie erreichen.
///
/// Ist ein Compact laut Policy fällig, läuft [`crate::compaction::compact_session`]
/// mit einem aus `policy.context_window_tokens()` abgeleiteten
/// [`crate::compaction::CompactionPlan`]; das Ergebnis wird bei Erfolg über
/// `store.save_history` persistiert (der Store-Standardimpl **ersetzt** den
/// gespeicherten Verlauf vollständig, siehe Doku von
/// [`crate::state_store::StateStore::save_history`] — kein Anhängen, daher
/// hier sicher verwendbar).
///
/// # Arguments
/// - `session` (`&mut AgentSession`): deren Historie ggf. ersetzt wird.
/// - `model` (`&dyn ModelProvider`): für den optionalen
///   Zusammenfassungs-Aufruf innerhalb von `compact_session`.
/// - `store` (`&dyn StateStore`): Ziel der Persistenz nach erfolgreichem
///   Compact.
/// - `last_round_usage` (`&harw_types::TokenUsage`): Nutzung der zuletzt
///   abgeschlossenen Modell-Runde.
/// - `task_completed` (`bool`): `true`, wenn der gerade beendete Abschnitt
///   einen Plan-Schritt abgeschlossen hat (siehe
///   [`turn_completed_a_plan_step`]).
///
/// # Concurrency
/// `async`; führt höchstens einen Modellaufruf aus (innerhalb von
/// `compact_session`) und einen Store-Aufruf. Fehler beider Seiten werden
/// nur geloggt — ein Compact-Fehlschlag darf den Turn nie abbrechen.
async fn maybe_compact(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    last_round_usage: &harw_types::TokenUsage,
    task_completed: bool,
) {
    let Some(policy) = session.auto_compact().copied() else {
        return;
    };

    // Siehe Funktionsdoku: provider-korrekte Prompt-Belegung inklusive
    // Cache (Anthropic meldet Cache-Tokens getrennt von `input_tokens`).
    let tokens_used = last_round_usage.prompt_tokens();
    let decision = policy.decide(tokens_used, task_completed);
    if !decision.should_compact() {
        return;
    }

    let mut plan =
        crate::compaction::CompactionPlan::for_context_window(policy.context_window_tokens());
    let (summary_provider, summary_model) = session.compaction_summary_model();
    plan.summary_provider = summary_provider.cloned();
    plan.summary_model = summary_model.cloned();
    match crate::compaction::compact_session(session, model, &plan, Some(decision)).await {
        Ok(outcome) => {
            tracing::info!(
                bytes_before = outcome.bytes_before,
                bytes_after = outcome.bytes_after,
                items_dropped = outcome.items_dropped,
                summarized = outcome.summarized,
                "turn_loop.auto_compact_applied",
            );
            if let Err(error) = store.save_history(session.id(), session.history()).await {
                tracing::warn!(%error, "turn_loop.auto_compact_save_history_failed");
            }
        }
        Err(error) => {
            tracing::warn!(%error, "turn_loop.auto_compact_failed");
        }
    }
}

/// Harte Verdichtung am Beginn eines neuen Auftrags-Turns einer bestehenden
/// Orchestrator-Session (Addendum D).
///
/// # Description
/// No-op, wenn die Session keine Policy trägt oder
/// [`crate::auto_compact::AutoCompactPolicy::turn_start_target_tokens`]
/// `None` ist (der Default; nur Root-/Sub-Orchestrator-Sessions setzen ihn,
/// siehe `child_controller.rs`). Sonst wird die Verlaufs-Token-Zahl aus
/// [`crate::compaction::estimated_history_tokens`] geschätzt; überschreitet
/// sie das Ziel, läuft [`crate::compaction::compact_session`] mit einem
/// [`crate::compaction::CompactionPlan::for_target_tokens`]-Plan und
/// [`crate::auto_compact::CompactDecision::TurnStart`] als Grund, bevor die
/// erste Modellrunde dieses Turns beginnt. Das Ergebnis wird bei Erfolg über
/// `store.save_history` persistiert, wie bei [`maybe_compact`].
///
/// # Arguments
/// - `session` (`&mut AgentSession`): deren Historie ggf. ersetzt wird.
/// - `model` (`&dyn ModelProvider`): für den optionalen
///   Zusammenfassungs-Aufruf innerhalb von `compact_session`.
/// - `store` (`&dyn StateStore`): Ziel der Persistenz nach erfolgreichem
///   Compact.
///
/// # Concurrency
/// `async`; höchstens ein Modellaufruf und ein Store-Aufruf. Fehler beider
/// Seiten werden nur geloggt — ein Fehlschlag darf den Turn nie abbrechen.
async fn maybe_hard_compact_at_turn_start(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
) {
    let Some(policy) = session.auto_compact().copied() else {
        return;
    };
    let Some(target_tokens) = policy.turn_start_target_tokens() else {
        return;
    };
    let estimated_tokens = crate::compaction::estimated_history_tokens(session.history());
    if estimated_tokens <= target_tokens {
        return;
    }

    let mut plan = crate::compaction::CompactionPlan::for_target_tokens(target_tokens);
    let (summary_provider, summary_model) = session.compaction_summary_model();
    plan.summary_provider = summary_provider.cloned();
    plan.summary_model = summary_model.cloned();
    match crate::compaction::compact_session(
        session,
        model,
        &plan,
        Some(crate::auto_compact::CompactDecision::TurnStart),
    )
    .await
    {
        Ok(outcome) => {
            tracing::info!(
                bytes_before = outcome.bytes_before,
                bytes_after = outcome.bytes_after,
                items_dropped = outcome.items_dropped,
                summarized = outcome.summarized,
                "turn_loop.turn_start_hard_compact_applied",
            );
            if let Err(error) = store.save_history(session.id(), session.history()).await {
                tracing::warn!(%error, "turn_loop.turn_start_hard_compact_save_history_failed");
            }
        }
        Err(error) => {
            tracing::warn!(%error, "turn_loop.turn_start_hard_compact_failed");
        }
    }
}

/// Prüft, ob der aktuelle Turn (alles ab der letzten `UserMessage`) einen
/// Plan-Schritt abgeschlossen hat.
///
/// # Description
/// Sucht rückwärts durch die Historie bis zur letzten `UserMessage` und
/// prüft jeden darin enthaltenen `ToolCall`, dessen `tool_name` mit `"plan"`
/// beginnt: gilt als „Plan-Schritt abgeschlossen", wenn die JSON-Argumente
/// (beliebig verschachtelt) ein Feld `"status"` mit dem Wert `"completed"`
/// oder `"done"` enthalten.
///
/// Zum Zeitpunkt dieser Implementierung existiert im Repository noch kein
/// konkretes Plan-Tool (`harw-plan`/`harw-plan-bridge` definieren bislang
/// keinen `ToolProvider`, siehe Bericht des Aufrufers) — Namenspräfix und
/// Argument-Form folgen daher wörtlich dem Vertrag, nicht einer bestehenden
/// Implementierung.
///
/// # Arguments
/// - `session` (`&AgentSession`): dessen Historie geprüft wird.
///
/// # Returns
/// `true`, wenn mindestens ein passender `ToolCall` im aktuellen Turn
/// gefunden wurde.
fn turn_completed_a_plan_step(session: &AgentSession) -> bool {
    let items = session.history().items();
    let turn_start = items
        .iter()
        .rposition(|item| matches!(item, TurnItem::UserMessage(_)))
        .unwrap_or(0);
    items[turn_start..].iter().any(|item| {
        let TurnItem::ToolCall(call) = item else {
            return false;
        };
        if !call.tool_name.starts_with("plan") {
            return false;
        }
        json_contains_completed_status(&call.arguments)
    })
}

/// Sucht rekursiv in einem `serde_json::Value` nach einem Feld `"status"`
/// mit dem Wert `"completed"` oder `"done"`.
fn json_contains_completed_status(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => map.iter().any(|(key, nested)| {
            if key == "status" {
                if let serde_json::Value::String(status) = nested {
                    if status == "completed" || status == "done" {
                        return true;
                    }
                }
            }
            json_contains_completed_status(nested)
        }),
        serde_json::Value::Array(items) => items.iter().any(json_contains_completed_status),
        _ => false,
    }
}

/// Beendet einen Turn nach einem [`TurnControl`]-Prüfpunkt-Treffer (Abbruch
/// oder Budget), bevor ein Modellaufruf gestartet wurde: keine offenen
/// Tool-Calls, deshalb kein Verlaufs-Nacharbeiten nötig.
///
/// # Beschreibung
/// Spiegelt den regulären Abschluss-Pfad am Ende von [`drive_turn`]
/// (`TurnAborted` statt `TurnCompleted`, sonst identisch): Observer werden
/// benachrichtigt und die Session kehrt über `AgentSession::complete_turn`
/// nach `Idle` zurück — ein Abbruch ist kein stiller Ausstieg, sondern ein
/// regulär abgeschlossener Turn mit `CancelReason` statt Erfolg.
///
/// # Errors
/// Reicht einen Fehler von `AgentSession::complete_turn` durch (z. B. wenn
/// `handle` nicht mehr der aktive Turn der Session ist).
async fn cancel_turn(
    session: &mut AgentSession,
    handle: TurnHandle,
    total_usage: harw_types::TokenUsage,
    reason: CancelReason,
) -> CoreResult<TurnOutcome> {
    emit(
        session,
        TurnEvent::TurnAborted {
            turn_id: handle.turn_id.clone(),
        },
    );
    notify_turn_stop(
        session,
        &TurnStopInput {
            session_id: handle.session_id.clone(),
            turn_id: handle.turn_id.clone(),
            token_usage: total_usage.clone(),
        },
    );
    session.complete_turn(handle, total_usage)?;
    Ok(TurnOutcome::Cancelled { reason })
}

/// Beendet einen Turn nach einem Provider-Stop-Grund, der kein reguläres
/// Turn-Ende ist ([`StopReason::MaxTokens`] nach ausgeschöpften
/// Auto-Continuations, [`StopReason::ContextWindowExceeded`],
/// [`StopReason::Refusal`], [`StopReason::ContentFilter`]).
///
/// # Beschreibung
/// Spiegelt den regulären Abschluss-Pfad am Ende von [`drive_turn`]
/// (Guard-Hinweis, Observer, `TurnCompleted`, `complete_turn`), endet aber mit
/// dem ehrlichen [`TurnOutcome::Truncated`]- bzw. [`TurnOutcome::Refused`]-Ausgang
/// statt `Completed`. Die bereits erhaltene Assistant-Antwort (inklusive
/// abgeschnittener Teiltext) ist zu diesem Zeitpunkt bereits persistiert; das
/// `complete_turn` hier sorgt dafür, dass die Session nach `Idle` zurückkehrt
/// und der nächste Turn sauber starten kann.
///
/// # Errors
/// Reicht einen Fehler von `AgentSession::complete_turn` durch (z. B. wenn
/// `handle` nicht mehr der aktive Turn der Session ist).
async fn finish_model_stop(
    session: &mut AgentSession,
    handle: TurnHandle,
    total_usage: harw_types::TokenUsage,
    pending_guard_hint: Option<String>,
    outcome: TurnOutcome,
) -> CoreResult<TurnOutcome> {
    // Welle FANIN-K: ein Hinweis, der am Ende der letzten Runde entstand,
    // aber keinem Tool-Ergebnis mehr zugeordnet werden konnte, geht auch an
    // dieser Endgrenze nicht verloren — er wird ans letzte Verlaufs-Item
    // angehängt (identisch zum regulären Abschluss-Pfad).
    if let Some(hint) = pending_guard_hint {
        session.history_mut().append_hint_to_last(&hint);
    }
    emit(
        session,
        TurnEvent::TurnCompleted {
            turn_id: handle.turn_id.clone(),
            usage: Some(total_usage.clone()),
        },
    );
    notify_turn_stop(
        session,
        &TurnStopInput {
            session_id: handle.session_id.clone(),
            turn_id: handle.turn_id.clone(),
            token_usage: total_usage.clone(),
        },
    );
    // Kein Auto-Compact an dieser Grenze: ein abgeschnittener oder
    // abgelehnter Turn ist keine sichere Kompaktionsgrenze; die Policy greift
    // spätestens am regulär abgeschlossenen Folge-Turn.
    if let Some(observer) = session.tool_outcome_observer().cloned() {
        observer.on_turn_finished(session.id());
    }
    session.complete_turn(handle, total_usage)?;
    Ok(outcome)
}

/// Wie [`cancel_turn`], aber für einen Treffer am Werkzeug-Prüfpunkt: `calls`
/// sind die noch nicht ausgeführten Tool-Calls der aktuellen Modellantwort.
///
/// # Beschreibung
/// Jeder Call bekommt zuerst seinen Aufruf-Eintrag (der Verlauf muss zeigen,
/// dass das Modell ihn angefordert hat) und dann ein synthetisches
/// Fehlerergebnis im Verlauf — ohne diese Paarung wäre der Verlauf beim
/// nächsten Modellaufruf nicht provider-gültig (ein `tool_call` ohne
/// zugehöriges `tool_result`). Anschließend endet der Turn wie [`cancel_turn`].
///
/// # Errors
/// Reicht Persistenzfehler (`persist_last`) und Fehler von [`cancel_turn`]
/// durch.
async fn cancel_turn_with_pending_calls(
    session: &mut AgentSession,
    store: &dyn StateStore,
    handle: TurnHandle,
    total_usage: harw_types::TokenUsage,
    reason: CancelReason,
    calls: Vec<ToolCall>,
) -> CoreResult<TurnOutcome> {
    for call in calls {
        session.history_mut().push_tool_call(
            call.id.clone(),
            call.name.to_string(),
            call.arguments.clone(),
        );
        persist_last(session, store).await?;
        session.history_mut().push_tool_result(
            call.id,
            ToolCallResult::error(format!("turn cancelled before execution ({reason:?})")),
            0,
        );
        persist_last(session, store).await?;
    }
    cancel_turn(session, handle, total_usage, reason).await
}

/// Execute a complete model response concurrently only when every call is an
/// explicitly parallel-safe ordinary tool and every guardrail allows it. Any
/// ambiguity falls back to the sequential path below; handoffs and approvals
/// are never raced.
///
/// # Beschreibung
/// Die Bedingungen werden in dieser Reihenfolge geprüft, und die Reihenfolge ist
/// Absicht:
/// 1. Mindestens zwei Calls, kein Handoff darunter.
/// 2. Für **jeden** Call ein `parallel_safe` markierter Executor
///    ([`find_parallel_executor`]).
/// 3. Ein aufgelöster Sandbox-Kontext.
/// 4. Erst danach die Approval-Vorprüfung über [`check_approval`].
///
/// Die Guardrail-Prüfung steht bewusst zuletzt: sie ist der einzige Schritt mit
/// Außenwirkung (Nutzer-Interaktion, Audit-Eintrag, vergebene `ItemId`). Würde
/// sie vor einer Bedingung stehen, die den Parallel-Pfad noch verwirft, hätte
/// ein Handler umsonst gefragt.
///
/// Früher genügte ein einziger registrierter `ApprovalHandler`, um die
/// Parallelisierung komplett abzuschalten. Da `harw-registry-defaults`
/// produktiv immer eine Policy registriert, war der `JoinSet`-Pfad damit nie
/// aktiv. Statt des Pauschalausschlusses prüft die Vorprüfung nun Call für
/// Call: nur wenn **alle** `Allow` liefern, wird parallel ausgeführt.
///
/// # Invariante (kein doppelter Handler-Aufruf)
/// Im Parallel-Pfad läuft [`check_approval`] genau einmal je Call und danach
/// nicht mehr — der sequenzielle Loop wird gar nicht erst betreten. Liefert ein
/// Call `Deny` oder `AskUser`, bricht die Vorprüfung ab und legt die bereits
/// eingeholten Entscheidungen in `prepared` ab; der sequenzielle Pfad verbraucht
/// sie dort, statt dieselben Handler ein zweites Mal zu fragen. In beiden
/// Fällen gilt: höchstens ein Handler-Aufruf pro Call und Modellantwort.
///
/// # Arguments
/// - `prepared` (`&mut PreparedApprovals`): Ausgabekanal für die Entscheidungen
///   der Vorprüfung. Wird nur beschrieben, wenn Schritt 4 erreicht wurde.
/// - `control` (`&TurnControl`): liefert den `CancelToken`, der an den
///   gemeinsamen `ToolExecutionContext` aller Jobs dieser Antwort angehängt
///   wird (siehe `tool_execution_context`). Ein Job, der daraufhin
///   `ToolsError::Cancelled` meldet, geht in der Ergebnis-Auslieferung
///   denselben `Aborted`-Weg wie ein `TurnGuard`-Abbruch, statt als normaler
///   Tool-Fehler ans Modell zurückgespielt zu werden.
///
/// # Returns
/// `true`, wenn die Antwort vollständig parallel ausgeführt und persistiert
/// wurde; `false`, wenn der Aufrufer den sequenziellen Pfad nehmen muss.
/// Ergebnis von [`try_execute_parallel_calls`] für die Turn-Wächter-Anbindung
/// (Addendum F+G): der Aufrufer (`drive_turn`) kennt `handle`/`total_usage`
/// und beendet einen `Aborted`-Turn deshalb selbst über [`cancel_turn`].
enum ParallelOutcome {
    /// Der Parallel-Pfad war nicht anwendbar — der Aufrufer muss den
    /// sequenziellen Pfad nehmen.
    NotApplicable,
    /// Alle Calls wurden parallel ausgeführt und persistiert.
    Executed,
    /// Ein `TurnGuard`-`Abort` ist mitten in der Ergebnisauslieferung
    /// aufgetreten; alle Ergebnisse sind bereits (ggf. synthetisch) im
    /// Verlauf gepaart.
    Aborted(CancelReason),
}

#[allow(clippy::too_many_arguments)]
async fn try_execute_parallel_calls(
    session: &mut AgentSession,
    store: &dyn StateStore,
    ctx: &TurnInputContext,
    calls: &[ToolCall],
    prepared: &mut PreparedApprovals,
    mut guard: Option<&mut TurnGuard>,
    seen_success_signatures: &mut HashSet<String>,
    pending_hint: &mut Option<String>,
    round_progressed: &mut bool,
    control: &TurnControl,
) -> CoreResult<ParallelOutcome> {
    if calls.len() < 2 || calls.iter().any(|call| handoff_role(&call.name).is_some()) {
        return Ok(ParallelOutcome::NotApplicable);
    }
    let mut jobs = Vec::with_capacity(calls.len());
    for call in calls {
        let Some(executor) = find_parallel_executor(session, &call.name) else {
            return Ok(ParallelOutcome::NotApplicable);
        };
        jobs.push((call.clone(), executor));
    }
    let execution_context = match tool_execution_context(session, ctx, control.cancel_token()) {
        Ok(context) => context,
        Err(error) => {
            missing_tool_execution_context_result(error)?;
            return Ok(ParallelOutcome::NotApplicable);
        }
    };
    if !preflight_approvals(session, calls, prepared).await {
        return Ok(ParallelOutcome::NotApplicable);
    }

    for (call, _) in &jobs {
        session.history_mut().push_tool_call(
            call.id.clone(),
            call.name.to_string(),
            call.arguments.clone(),
        );
        persist_last(session, store).await?;
        emit(
            session,
            TurnEvent::ToolCallRequested {
                turn_id: ctx.turn_id.clone(),
                call_id: call.id.clone(),
                tool_name: call.name.to_string(),
                arguments: call.arguments.clone(),
            },
        );
    }

    // Fix C (Moduldoku „Siebter Nachtrag"): Metadaten je Position VOR dem
    // Spawn-Loop mitschneiden, der `jobs` konsumiert — der spätere
    // Cancel-Fill-in unten braucht `call_id`/`tool_name`/`arguments` je
    // Slot, kann sie nach `jobs.into_iter()` aber nicht mehr aus `jobs`
    // lesen.
    let call_meta: Vec<(ToolCallId, String, serde_json::Value)> = jobs
        .iter()
        .map(|(call, _executor)| {
            (
                call.id.clone(),
                call.name.to_string(),
                call.arguments.clone(),
            )
        })
        .collect();
    let mut joins = tokio::task::JoinSet::new();
    for (position, (call, executor)) in jobs.into_iter().enumerate() {
        let execution_context = execution_context.clone();
        let tool_name = call.name.to_string();
        let tool_span = tracing::info_span!("tool.call", tool_name = %tool_name);
        let arguments_for_capture = call.arguments.clone();
        let tool_name_for_capture = tool_name.clone();
        joins.spawn(
            async move {
                let started = std::time::Instant::now();
                let output = executor.traced_execute(&execution_context, &call).await;
                let cancelled = matches!(output, Err(ToolsError::Cancelled));
                let result = output
                    .map(output_to_result)
                    .unwrap_or_else(|error| ToolCallResult::error(error.to_string()));
                let duration_ms = started.elapsed().as_millis() as u64;
                tracing::info!(
                    duration_ms = duration_ms,
                    status = if result.is_success() { "ok" } else { "err" },
                    "tool.execute",
                );
                (
                    position,
                    call.id,
                    result,
                    duration_ms,
                    tool_name_for_capture,
                    arguments_for_capture,
                    cancelled,
                )
            }
            .instrument(tool_span),
        );
    }
    let mut results: Vec<ParallelCallSlot> = vec![None; calls.len()];
    // Fix C (Moduldoku „Siebter Nachtrag"): dieser Join-Loop wartete bisher
    // ungeraced auf **alle** Aufrufe, bevor überhaupt auf Cancel reagiert
    // wurde — ein Ausführer, der `ctx.cancel()` selbst nie abfragt, blockierte
    // die gesamte Antwort bis zu seinem eigenen Ende. Jetzt racet die
    // Reaper-Schleife selbst gegen `control`s `CancelToken`: ein Treffer
    // bricht alle noch laufenden Jobs über `joins.abort_all()` ab, statt
    // weiter auf sie zu warten.
    let mut cancel_hit = false;
    loop {
        tokio::select! {
            biased;
            () = control.cancel_token().cancelled(), if !cancel_hit => {
                cancel_hit = true;
                joins.abort_all();
            }
            joined = joins.join_next() => {
                match joined {
                    Some(Ok((position, call_id, result, duration_ms, tool_name, arguments, cancelled))) => {
                        results[position] =
                            Some((call_id, result, duration_ms, tool_name, arguments, cancelled));
                    }
                    Some(Err(error)) => {
                        // `abort_all()` selbst kann noch laufende Jobs als
                        // `Err(JoinError)` zurückliefern — erwartet, sobald
                        // `cancel_hit` bereits `true` ist: ihr `None`-Slot
                        // wird gleich unten synthetisch befüllt, das Ergebnis
                        // hier wird verworfen. Ohne vorherigen Cancel-Treffer
                        // ist ein `JoinError` (z. B. ein echter Panic) weiterhin
                        // ein echter Fehler dieser Funktion.
                        if !cancel_hit {
                            return Err(CoreError::ToolFailed(error.to_string()));
                        }
                    }
                    None => break,
                }
            }
        }
    }
    if cancel_hit {
        for (position, slot) in results.iter_mut().enumerate() {
            if slot.is_none() {
                let (call_id, tool_name, arguments) = call_meta[position].clone();
                *slot = Some((
                    call_id,
                    ToolCallResult::error(format!(
                        "turn cancelled before delivery ({:?})",
                        control
                            .cancel_token()
                            .reason()
                            .unwrap_or(CancelReason::User)
                    )),
                    0,
                    tool_name,
                    arguments,
                    true, // cancelled
                ));
            }
        }
    }
    // `while let` über den Positions-Iterator statt `for`: bei einem
    // `TurnGuard`-`Abort` mitten in der Auslieferung (Addendum F+G) brauchen
    // die noch nicht ausgelieferten, aber bereits abgeschlossenen Ergebnisse
    // weiterhin ein synthetisches `tool_result` — ihr `tool_call` steht schon
    // im Verlauf (oben, vor dem Start der parallelen Ausführung).
    let mut results_iter = results.into_iter();
    let mut aborted: Option<CancelReason> = None;
    for result in results_iter.by_ref() {
        let (call_id, mut value, duration_ms, tool_name, arguments, cancelled) = result
            .ok_or_else(|| {
                CoreError::ToolFailed("parallel tool scheduler lost a completed call".to_owned())
            })?;
        // `ToolsError::Cancelled` (der `ToolExecutionContext` trägt `control`s
        // `CancelToken`, siehe der Konstruktion oben) geht denselben Weg wie
        // ein `TurnGuard`-`Abort`: kein normaler Tool-Fehler ans Modell,
        // sondern derselbe Prüfpunkt-Treffer-Pfad — die Wächter-Beratung
        // (`apply_tool_guard`) wird für diesen Call übersprungen, sein bereits
        // korrekt geformtes Cancel-Ergebnis (`value`) wird unverändert
        // ausgeliefert.
        let abort_reason = if cancelled {
            Some(
                control
                    .cancel_token()
                    .reason()
                    .unwrap_or(CancelReason::User),
            )
        } else {
            apply_tool_guard(
                session,
                store,
                guard.as_deref_mut(),
                seen_success_signatures,
                pending_hint,
                round_progressed,
                &tool_name,
                &arguments,
                &mut value,
            )
            .await
        };
        emit(
            session,
            TurnEvent::ToolCallCompleted {
                turn_id: ctx.turn_id.clone(),
                call_id: call_id.clone(),
                result: value.clone(),
                duration_ms,
            },
        );
        notify_tool_outcome(session, &tool_name, &arguments, &value);
        notify_tool_progress(session);
        session
            .history_mut()
            .push_tool_result(call_id, value, duration_ms);
        persist_last(session, store).await?;
        if let Some(reason) = abort_reason {
            aborted = Some(reason);
            break;
        }
    }
    if let Some(reason) = aborted {
        for result in results_iter {
            let (call_id, _value, _duration_ms, _tool_name, _arguments, _cancelled) = result
                .ok_or_else(|| {
                    CoreError::ToolFailed(
                        "parallel tool scheduler lost a completed call".to_owned(),
                    )
                })?;
            session.history_mut().push_tool_result(
                call_id,
                ToolCallResult::error(format!("turn cancelled before delivery ({reason:?})")),
                0,
            );
            persist_last(session, store).await?;
        }
        return Ok(ParallelOutcome::Aborted(reason));
    }
    Ok(ParallelOutcome::Executed)
}

/// Holt für jeden Call die Guardrail-Entscheidung ein, bis eine davon nicht
/// `Allow` ist.
///
/// # Beschreibung
/// Die Entscheidung je Call stammt aus [`check_approval`] und ist damit
/// dieselbe Aggregation über **alle** Handler (`Deny` > `AskUser` > `Allow`)
/// wie im sequenziellen Pfad — ein später registriertes `Deny` kann auch hier
/// nicht von einem früheren `AskUser` verdeckt werden.
///
/// Jede eingeholte Entscheidung — auch die abschlägige — wird in `prepared`
/// hinterlegt, damit der sequenzielle Fallback sie verbrauchen kann statt
/// denselben Handler erneut zu fragen. Ohne registrierten `ApprovalHandler`
/// liefert [`check_approval`] `Allow`, ohne dass irgendetwas aufgerufen wird;
/// die Vorprüfung ist dann ein reiner Durchlauf.
///
/// # Arguments
/// - `calls` (`&[ToolCall]`): die Calls der Modellantwort in Originalreihenfolge.
/// - `prepared` (`&mut PreparedApprovals`): wird überschrieben.
///
/// # Returns
/// `true`, wenn **alle** Calls `Allow` bekommen haben.
async fn preflight_approvals(
    session: &AgentSession,
    calls: &[ToolCall],
    prepared: &mut PreparedApprovals,
) -> bool {
    prepared.decisions = Vec::with_capacity(calls.len());
    for call in calls {
        let decision = check_approval(session, call).await;
        let allowed = matches!(decision, ApprovalDecision::Allow);
        prepared.decisions.push(Some(decision));
        if !allowed {
            return false;
        }
    }
    true
}

/// Persistiert das zuletzt angehängte History-Item über den `StateStore`.
///
/// Emits a `transcript.persist` debug event with `records_written = 1` when a
/// record is actually saved, or `records_written = 0` when the history is empty.
/// Never logs the content of the record (redaction-by-default).
async fn persist_last(session: &AgentSession, store: &dyn StateStore) -> CoreResult<()> {
    if let Some(item) = session.history().last() {
        store
            .save_turn(session.id(), item)
            .await
            .map_err(|error| state_store_error(error, "persistence"))?;
        tracing::debug!(records_written = 1u64, "transcript.persist");
    } else {
        tracing::debug!(records_written = 0u64, "transcript.persist");
    }
    Ok(())
}

fn state_store_error(error: StateStoreError, operation_name: &str) -> CoreError {
    match error {
        StateStoreError::PoisonedMutex { operation } => CoreError::TurnRejected(format!(
            "state store {operation_name} failed during {operation}"
        )),
        StateStoreError::SessionStore(error) => CoreError::SessionStore(error),
        StateStoreError::SequenceExhausted { session } => CoreError::TurnRejected(format!(
            "state store {operation_name} failed: transcript sequence exhausted for session {}",
            session.as_str()
        )),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activation::{SessionActivation, ToolProfile};
    use crate::session::AgentSession;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::SandboxSpec;
    use harw_catalog::AgentSuggestions;
    use harw_extension_api::contributors::ToolProvider;
    use harw_extension_api::{
        AgentSpawnError, AgentSpawner, ExtensionRegistryBuilder, SpawnFuture, SpawnInput,
    };
    use harw_tools::{
        FunctionToolSpec, JsonSchema, ToolCall, ToolExecutionContext, ToolExecutor,
        ToolExecutorFuture, ToolName, ToolOutput, ToolSpec, ToolsError,
    };
    use harw_types::{AgentRole, TurnId};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    struct LoadFailStore;

    #[test]
    fn approval_resolution_timed_out_is_a_reject_with_the_canonical_reason() {
        assert_eq!(
            ApprovalResolution::timed_out(),
            ApprovalResolution::Reject {
                reason: crate::session::APPROVAL_TIMEOUT_REASON.to_owned(),
            }
        );
    }

    #[test]
    fn sequence_exhaustion_rejects_the_turn() {
        let session = SessionId::new();
        let error = state_store_error(
            StateStoreError::SequenceExhausted {
                session: session.clone(),
            },
            "persistence",
        );

        assert!(matches!(
            error,
            CoreError::TurnRejected(reason)
                if reason.contains("state store persistence failed")
                    && reason.contains(session.as_str())
        ));
    }

    impl StateStore for LoadFailStore {
        fn save_turn<'a>(
            &'a self,
            _sid: &'a SessionId,
            _item: &'a TurnItem,
        ) -> harw_extension_api::ExtFuture<'a, crate::state_store::StateStoreResult<()>> {
            Box::pin(async { Ok(()) })
        }

        fn load_history<'a>(
            &'a self,
            _sid: &'a SessionId,
        ) -> harw_extension_api::ExtFuture<
            'a,
            crate::state_store::StateStoreResult<crate::history::ConversationHistory>,
        > {
            Box::pin(async {
                Err(crate::state_store::StateStoreError::PoisonedMutex {
                    operation: "test_load_history",
                })
            })
        }
    }

    // Always fails with a retryable `ModelError::RateLimited`, so tests can
    // assert `transition_after_turn_failure` returns the session to `Idle`.
    struct RateLimitedModel;

    impl ModelProvider for RateLimitedModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
            Box::pin(async {
                Err(crate::model::ModelError::RateLimited {
                    retry_after_secs: 30,
                    message: "test provider limit".to_owned(),
                })
            })
        }
    }

    // Always fails with a retryable `ModelError::Transient` (HTTP 500), the
    // real-world Cloudflare AI Gateway guardrails case that exposed the bug
    // in `transition_after_turn_failure`; tests assert it also resolves to
    // `Idle`, not just `RateLimited`.
    struct TransientModel;

    impl ModelProvider for TransientModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
            Box::pin(async {
                Err(crate::model::ModelError::Transient {
                    status: Some(500),
                    retry_after_secs: None,
                    message: "gateway guardrails block".to_owned(),
                })
            })
        }
    }

    // Always fails with a retryable `ModelError::Timeout`, so tests can
    // assert `transition_after_turn_failure` returns the session to `Idle`.
    struct TimeoutModel;

    impl ModelProvider for TimeoutModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
            Box::pin(async {
                Err(crate::model::ModelError::Timeout {
                    message: "test provider timeout".to_owned(),
                })
            })
        }
    }

    // Always fails with a non-retryable `ModelError::Refusal`, so tests can
    // assert `transition_after_turn_failure` moves the session to the
    // terminal `Failed` state instead of `Idle`.
    struct RefusalModel;

    impl ModelProvider for RefusalModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
            Box::pin(async {
                Err(crate::model::ModelError::Refusal {
                    detail: Some("policy violation".to_owned()),
                })
            })
        }
    }

    // Always fails with a non-retryable `ModelError::QuotaExceeded`, so tests
    // can assert `transition_after_turn_failure` moves the session to the
    // terminal `Failed` state, not `Idle` — a provider exhausting its
    // contingent is not a transient condition a retry can fix, and this is
    // an easy variant to accidentally make retryable later.
    struct QuotaExceededModel;

    impl ModelProvider for QuotaExceededModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
            Box::pin(async {
                Err(crate::model::ModelError::QuotaExceeded {
                    message: "test provider quota exhausted".to_owned(),
                })
            })
        }
    }

    struct MissingContextRecoveryModel {
        call_id: ToolCallId,
        responses: AtomicUsize,
        saw_rejected_tool_result: AtomicBool,
    }

    impl ModelProvider for MissingContextRecoveryModel {
        fn respond<'a>(&'a self, request: ModelRequest) -> crate::model::ModelFuture<'a> {
            let response_index = self.responses.fetch_add(1, Ordering::SeqCst);
            let call_id = self.call_id.clone();
            if response_index > 0 {
                self.saw_rejected_tool_result.store(
                    request.history.items().iter().any(|item| {
                        matches!(item, TurnItem::ToolResult(result) if !result.result.is_success())
                    }),
                    Ordering::SeqCst,
                );
            }
            Box::pin(async move {
                if response_index == 0 {
                    Ok(crate::model::ModelResponse {
                        message: None,
                        tool_calls: vec![ToolCall {
                            id: call_id,
                            name: ToolName::new("fs.read"),
                            arguments: serde_json::json!({"path": "untrusted"}),
                        }],
                        usage: Default::default(),
                        ..Default::default()
                    })
                } else {
                    Ok(crate::model::ModelResponse::text(
                        "I can continue without that tool.",
                    ))
                }
            })
        }
    }

    struct CompletionTrackingSpawner {
        child_completed: AtomicUsize,
        child_finished: AtomicUsize,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl AgentSpawner for CompletionTrackingSpawner {
        fn spawn_child<'a>(
            &'a self,
            _role: &'a str,
            _input: SpawnInput,
            _sandbox: SandboxSpec,
            _suggestions: Option<AgentSuggestions>,
        ) -> SpawnFuture<'a> {
            Box::pin(async { Ok(SessionId::new()) })
        }

        fn child_finished(&self, _child: &SessionId) {
            self.child_finished.fetch_add(1, Ordering::SeqCst);
        }

        fn child_completed(
            &self,
            _child: &SessionId,
            _completed_at: jiff::Timestamp,
        ) -> Result<(), AgentSpawnError> {
            let mut events = self
                .events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            assert_eq!(events.as_slice(), ["persisted_child_result"]);
            events.push("durably_completed_child");
            self.child_completed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct ChildResultStore {
        fail_child_result: bool,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl StateStore for ChildResultStore {
        fn save_turn<'a>(
            &'a self,
            _sid: &'a SessionId,
            item: &'a TurnItem,
        ) -> harw_extension_api::ExtFuture<'a, crate::state_store::StateStoreResult<()>> {
            let is_child_result = matches!(item, TurnItem::ToolResult(_));
            let fail_child_result = self.fail_child_result;
            let events = self.events.clone();
            Box::pin(async move {
                if is_child_result && fail_child_result {
                    return Err(crate::state_store::StateStoreError::PoisonedMutex {
                        operation: "child_result_persistence",
                    });
                }
                if is_child_result {
                    events
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push("persisted_child_result");
                }
                Ok(())
            })
        }

        fn load_history<'a>(
            &'a self,
            _sid: &'a SessionId,
        ) -> harw_extension_api::ExtFuture<
            'a,
            crate::state_store::StateStoreResult<crate::history::ConversationHistory>,
        > {
            Box::pin(async { Ok(crate::history::ConversationHistory::new()) })
        }
    }

    fn paused_child_session(
        spawner: Arc<CompletionTrackingSpawner>,
    ) -> TestResult<(AgentSession, SessionId, ToolCallId)> {
        let (tx, _rx) = mpsc::unbounded_channel();
        let registry = ExtensionRegistryBuilder::default().spawner(spawner).build();
        let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx);
        session.try_start_turn().map_err(ctx("test turn starts"))?;
        let child = SessionId::new();
        let call_id = ToolCallId::new();
        session
            .begin_handoff(child.clone(), call_id.clone(), "worker".to_owned())
            .map_err(ctx("test handoff pauses"))?;
        Ok((session, child, call_id))
    }

    #[tokio::test]
    async fn durable_child_completion_follows_persisted_child_result() -> TestResult {
        let events = Arc::new(Mutex::new(Vec::new()));
        let spawner = Arc::new(CompletionTrackingSpawner {
            child_completed: AtomicUsize::new(0),
            child_finished: AtomicUsize::new(0),
            events: events.clone(),
        });
        let (mut session, child, call_id) = paused_child_session(spawner.clone())?;
        let store = ChildResultStore {
            fail_child_result: false,
            events: events.clone(),
        };
        let temp = tempfile::tempdir().map_err(ctx("temporary approval directory"))?;
        let approvals = ApprovalStore::new(temp.path());

        let outcome = resume_after_child_durable(
            &mut session,
            &crate::model::EchoModelProvider::new("parent resumed"),
            &store,
            &approvals,
            child,
            call_id,
            ToolCallResult::success(serde_json::json!({"child": "done"})),
        )
        .await
        .map_err(ctx("durable child result resumes the parent"))?;

        assert!(matches!(outcome, TurnOutcome::Completed));
        assert_eq!(spawner.child_completed.load(Ordering::SeqCst), 1);
        assert_eq!(spawner.child_finished.load(Ordering::SeqCst), 0);
        assert_eq!(
            events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_slice(),
            ["persisted_child_result", "durably_completed_child"]
        );
        Ok(())
    }

    #[tokio::test]
    async fn failed_child_result_persistence_does_not_release_durable_child() -> TestResult {
        let events = Arc::new(Mutex::new(Vec::new()));
        let spawner = Arc::new(CompletionTrackingSpawner {
            child_completed: AtomicUsize::new(0),
            child_finished: AtomicUsize::new(0),
            events: events.clone(),
        });
        let (mut session, child, call_id) = paused_child_session(spawner.clone())?;
        let store = ChildResultStore {
            fail_child_result: true,
            events: events.clone(),
        };
        let temp = tempfile::tempdir().map_err(ctx("temporary approval directory"))?;
        let approvals = ApprovalStore::new(temp.path());

        let Err(error) = resume_after_child_durable(
            &mut session,
            &crate::model::EchoModelProvider::new("must not run"),
            &store,
            &approvals,
            child,
            call_id,
            ToolCallResult::success(serde_json::json!({"child": "done"})),
        )
        .await
        else {
            return Err(TestError::Unexpected(
                "failed child-result persistence rejects the resume".to_owned(),
            ));
        };

        assert!(matches!(error, CoreError::TurnRejected(_)));
        assert_eq!(spawner.child_completed.load(Ordering::SeqCst), 0);
        assert_eq!(spawner.child_finished.load(Ordering::SeqCst), 0);
        assert!(
            events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_empty(),
            "a failed persistence attempt must not release the child"
        );
        Ok(())
    }

    // ------------------------------------------------------------------
    // Minimal stub ToolProvider
    // ------------------------------------------------------------------

    struct StubToolProvider {
        specs: Vec<ToolSpec>,
    }

    impl StubToolProvider {
        fn with_names(names: &[&str]) -> Arc<Self> {
            let specs = names
                .iter()
                .map(|n| {
                    ToolSpec::Function(FunctionToolSpec {
                        name: ToolName::new(*n),
                        description: format!("stub tool {n}"),
                        parameters: JsonSchema::default(),
                        strict: false,
                    })
                })
                .collect();
            Arc::new(Self { specs })
        }
    }

    impl ToolProvider for StubToolProvider {
        fn tools(&self) -> Vec<ToolSpec> {
            self.specs.clone()
        }
        fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
            if self.specs.iter().any(|s| s.name() == name.as_str()) {
                Some(Arc::new(StubExecutor))
            } else {
                None
            }
        }
    }

    struct StubExecutor;

    impl ToolExecutor for StubExecutor {
        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolExecutionContext,
            _call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async {
                Ok(ToolOutput::Text {
                    content: "ok".to_owned(),
                })
            })
        }
    }

    fn make_session(
        provider: Arc<dyn ToolProvider>,
        activation: SessionActivation,
    ) -> AgentSession {
        let (tx, _rx) = mpsc::unbounded_channel();
        let registry = ExtensionRegistryBuilder::default()
            .tool_provider(provider)
            .build();
        AgentSession::new(AgentRole::Assistant, None, registry, tx).with_activation(activation)
    }

    // ------------------------------------------------------------------
    // collect_tools filter tests
    // ------------------------------------------------------------------

    #[test]
    fn test_collect_tools_full_profile_includes_all() -> TestResult {
        let provider = StubToolProvider::with_names(&["fs.read", "custom.tool"]);
        let session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let tools = collect_tools(&session).map_err(ctx("collect_tools should not fail"))?;
        let names: Vec<_> = tools.iter().map(|t| t.name().to_owned()).collect();
        assert!(names.contains(&"fs.read".to_owned()));
        assert!(names.contains(&"custom.tool".to_owned()));
        Ok(())
    }

    #[test]
    fn test_collect_tools_coding_profile_excludes_unlisted() -> TestResult {
        let provider = StubToolProvider::with_names(&["fs.read", "custom.tool"]);
        let session = make_session(provider, SessionActivation::new(ToolProfile::Coding));
        let tools = collect_tools(&session).map_err(ctx("collect_tools should not fail"))?;
        let names: Vec<_> = tools.iter().map(|t| t.name().to_owned()).collect();
        assert!(
            names.contains(&"fs.read".to_owned()),
            "fs.read must be visible"
        );
        assert!(
            !names.contains(&"custom.tool".to_owned()),
            "custom.tool must be hidden by Coding profile"
        );
        Ok(())
    }

    #[test]
    fn test_collect_tools_disable_override_hides_allowlisted_tool() -> TestResult {
        let provider = StubToolProvider::with_names(&["fs.read", "fs.write"]);
        let mut activation = SessionActivation::new(ToolProfile::Full);
        activation.disable_tool(ToolName::new("fs.write"));
        let session = make_session(provider, activation);
        let tools = collect_tools(&session).map_err(ctx("collect_tools should not fail"))?;
        let names: Vec<_> = tools.iter().map(|t| t.name().to_owned()).collect();
        assert!(names.contains(&"fs.read".to_owned()));
        assert!(
            !names.contains(&"fs.write".to_owned()),
            "fs.write must be hidden by override"
        );
        Ok(())
    }

    #[test]
    fn test_collect_tools_sorted_by_name() -> TestResult {
        // Absichtlich unsortiert registriert — die Provider-Reihenfolge darf
        // die Reihenfolge im Tool-Array nicht bestimmen (Prompt-Cache).
        let provider = StubToolProvider::with_names(&["zeta.tool", "alpha.tool", "mid.tool"]);
        let session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let tools = collect_tools(&session).map_err(ctx("collect_tools should not fail"))?;
        let names: Vec<_> = tools.iter().map(ToolSpec::name).collect();
        assert_eq!(names, vec!["alpha.tool", "mid.tool", "zeta.tool"]);
        Ok(())
    }

    // ------------------------------------------------------------------
    // find_executor filter tests
    // ------------------------------------------------------------------

    #[test]
    fn test_find_executor_returns_none_when_tool_disabled() {
        let provider = StubToolProvider::with_names(&["shell.exec"]);
        let mut activation = SessionActivation::new(ToolProfile::Full);
        activation.disable_tool(ToolName::new("shell.exec"));
        let session = make_session(provider, activation);
        let result = find_executor(&session, &ToolName::new("shell.exec"));
        assert!(result.is_none(), "disabled tool must have no executor");
    }

    #[test]
    fn test_find_executor_returns_some_when_tool_enabled() {
        let provider = StubToolProvider::with_names(&["fs.read"]);
        let session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let result = find_executor(&session, &ToolName::new("fs.read"));
        assert!(result.is_some(), "enabled tool must return an executor");
    }

    // Fix C (Moduldoku „Achter Nachtrag"): `shell.exec` is one of
    // `crate::guard::POLLING_TOOLS` — repeating the exact same successful
    // call is legitimate while waiting on a long-running background job, so
    // every repeat must still count as round progress. Before the fix, only
    // the *first* occurrence of a call signature counted, so a model
    // legitimately polling a background process saw the "no progress" hint
    // and misread it as proof of a response cache.
    #[tokio::test]
    async fn apply_tool_guard_repeated_shell_exec_success_always_counts_as_progress() {
        let provider = StubToolProvider::with_names(&["shell.exec"]);
        let session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();
        let mut guard = TurnGuard::new(crate::guard::GuardPolicy::default(), session.id());
        let mut seen_success_signatures: HashSet<String> = HashSet::new();
        let args = serde_json::json!({"cmd": "jobs -l"});

        // More rounds than the default `no_progress_rounds_abort` (8), to
        // prove polling never trips the guard, not merely within a narrow
        // window.
        for round in 0..10 {
            let mut round_progressed = false;
            let mut pending_hint = None;
            let mut result = ToolCallResult::success(serde_json::json!("still running"));
            let abort_reason = apply_tool_guard(
                &session,
                &store,
                Some(&mut guard),
                &mut seen_success_signatures,
                &mut pending_hint,
                &mut round_progressed,
                "shell.exec",
                &args,
                &mut result,
            )
            .await;
            assert!(abort_reason.is_none(), "round {round}: unexpected abort");
            assert!(
                round_progressed,
                "round {round}: a repeated successful shell.exec call must count as progress"
            );
            assert!(
                matches!(
                    guard.observe_round_end(round_progressed),
                    GuardVerdict::Continue
                ),
                "round {round}: polling a running process must never trigger the no-progress hint"
            );
        }
    }

    // Companion to the polling exemption above: an *unexempted* tool
    // (`fs.read`) repeated with identical arguments must still trip the
    // no-progress warning within the default threshold — the fix narrows the
    // exemption to `crate::guard::POLLING_TOOLS`, it does not weaken the
    // guard generally.
    #[tokio::test]
    async fn apply_tool_guard_repeated_fs_read_success_still_warns_no_progress() {
        let provider = StubToolProvider::with_names(&["fs.read"]);
        let session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();
        let mut guard = TurnGuard::new(crate::guard::GuardPolicy::default(), session.id());
        let mut seen_success_signatures: HashSet<String> = HashSet::new();
        let args = serde_json::json!({"path": "same.txt"});

        let mut saw_warn = false;
        for _ in 0..crate::guard::GuardPolicy::default().no_progress_rounds_warn + 1 {
            let mut round_progressed = false;
            let mut pending_hint = None;
            let mut result = ToolCallResult::success(serde_json::json!("contents"));
            let _ = apply_tool_guard(
                &session,
                &store,
                Some(&mut guard),
                &mut seen_success_signatures,
                &mut pending_hint,
                &mut round_progressed,
                "fs.read",
                &args,
                &mut result,
            )
            .await;
            if matches!(
                guard.observe_round_end(round_progressed),
                GuardVerdict::Warn { .. }
            ) {
                saw_warn = true;
                break;
            }
        }
        assert!(
            saw_warn,
            "repeated identical fs.read calls must still trigger the no-progress hint \
             within the default threshold"
        );
    }

    #[tokio::test]
    async fn rate_limited_model_failure_leaves_the_session_retryable() -> TestResult {
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();

        let Err(error) = run_turn(
            &mut session,
            &RateLimitedModel,
            &store,
            TurnInput::user("retry me"),
        )
        .await
        else {
            return Err(TestError::Unexpected(
                "rate limiting reaches the caller".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            CoreError::Model(crate::model::ModelError::RateLimited { .. })
        ));
        assert!(
            matches!(session.state(), crate::session::SessionState::Idle),
            "a retryable provider failure must not terminalize the session"
        );

        let outcome = run_turn(
            &mut session,
            &crate::model::EchoModelProvider::new("recovered"),
            &store,
            TurnInput::user("try again"),
        )
        .await
        .map_err(ctx("the same session accepts a later retry"))?;
        assert!(matches!(outcome, TurnOutcome::Completed));
        Ok(())
    }

    #[tokio::test]
    async fn transient_model_failure_leaves_the_session_retryable() -> TestResult {
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();

        let Err(error) = run_turn(
            &mut session,
            &TransientModel,
            &store,
            TurnInput::user("retry me"),
        )
        .await
        else {
            return Err(TestError::Unexpected(
                "a transient provider failure reaches the caller".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            CoreError::Model(crate::model::ModelError::Transient { .. })
        ));
        assert!(
            matches!(session.state(), crate::session::SessionState::Idle),
            "a transient (e.g. HTTP 500 guardrails block) provider failure must not terminalize the session"
        );

        let outcome = run_turn(
            &mut session,
            &crate::model::EchoModelProvider::new("recovered"),
            &store,
            TurnInput::user("try again"),
        )
        .await
        .map_err(ctx("the same session accepts a later retry"))?;
        assert!(matches!(outcome, TurnOutcome::Completed));
        Ok(())
    }

    #[tokio::test]
    async fn timeout_model_failure_leaves_the_session_retryable() -> TestResult {
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();

        let Err(error) = run_turn(
            &mut session,
            &TimeoutModel,
            &store,
            TurnInput::user("retry me"),
        )
        .await
        else {
            return Err(TestError::Unexpected(
                "a provider timeout reaches the caller".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            CoreError::Model(crate::model::ModelError::Timeout { .. })
        ));
        assert!(
            matches!(session.state(), crate::session::SessionState::Idle),
            "a provider timeout must not terminalize the session"
        );

        let outcome = run_turn(
            &mut session,
            &crate::model::EchoModelProvider::new("recovered"),
            &store,
            TurnInput::user("try again"),
        )
        .await
        .map_err(ctx("the same session accepts a later retry"))?;
        assert!(matches!(outcome, TurnOutcome::Completed));
        Ok(())
    }

    #[tokio::test]
    async fn refusal_model_failure_terminalizes_the_session() -> TestResult {
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();

        let Err(error) = run_turn(
            &mut session,
            &RefusalModel,
            &store,
            TurnInput::user("do the thing"),
        )
        .await
        else {
            return Err(TestError::Unexpected(
                "a refusal reaches the caller".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            CoreError::Model(crate::model::ModelError::Refusal { .. })
        ));
        assert!(
            matches!(session.state(), crate::session::SessionState::Failed(_)),
            "a non-retryable provider failure (Refusal) must still terminalize the session"
        );
        Ok(())
    }

    // `transition_after_turn_failure`'s retryable branch calls
    // `session.complete_turn(...)` to return the session to `Idle`. That call
    // itself can be rejected (e.g. a `TurnHandle` that no longer names the
    // session's active turn) — a rare, best-effort case per the function's
    // doc comment: the session is left in its prior state rather than
    // inventing a state transition the caller never asked for. Driving this
    // through the public `run_turn` API isn't practical (nothing in that
    // path can make the turn handle stale mid-flight), so this test calls
    // the private function directly with a `TurnInputContext` whose
    // `turn_id` deliberately does not match the session's actual active
    // turn, forcing `complete_turn` to reject it.
    #[test]
    fn transition_after_turn_failure_logs_but_does_not_panic_when_complete_turn_itself_fails()
    -> TestResult {
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));

        let real_handle = session.try_start_turn().map_err(ctx(
            "a freshly created session starts Idle and accepts a turn",
        ))?;
        assert!(matches!(
            session.state(),
            crate::session::SessionState::Running
        ));

        // Same session, but a turn id that is not the one `try_start_turn`
        // actually activated — `complete_turn` must reject this handle.
        let stale_ctx = harw_extension_api::TurnInputContext {
            session_id: real_handle.session_id.clone(),
            turn_id: TurnId::new(),
            metadata: serde_json::Value::Null,
        };
        let retryable_error = CoreError::Model(crate::model::ModelError::RateLimited {
            retry_after_secs: 1,
            message: "test provider limit".to_owned(),
        });

        transition_after_turn_failure(&mut session, &stale_ctx, &retryable_error);

        // The original error was surfaced (via `tracing::error!` inside
        // `transition_after_turn_failure`, not asserted here directly), and
        // the session was neither corrupted nor silently marked `Idle` or
        // `Failed` — it stays exactly where `complete_turn` left it: still
        // `Running`, still owning its real, original turn.
        assert!(
            matches!(session.state(), crate::session::SessionState::Running),
            "a failed best-effort completion must leave the session in its prior state"
        );
        assert_eq!(
            session.current_turn(),
            Some(&real_handle.turn_id),
            "the session's real active turn must be untouched by the rejected completion"
        );
        Ok(())
    }

    #[tokio::test]
    async fn quota_exceeded_model_failure_terminalizes_the_session() -> TestResult {
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();

        let Err(error) = run_turn(
            &mut session,
            &QuotaExceededModel,
            &store,
            TurnInput::user("do the thing"),
        )
        .await
        else {
            return Err(TestError::Unexpected(
                "a quota-exceeded failure reaches the caller".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            CoreError::Model(crate::model::ModelError::QuotaExceeded { .. })
        ));
        assert!(
            matches!(session.state(), crate::session::SessionState::Failed(_)),
            "an exhausted quota is not a transient condition a retry can fix, \
             so the session must terminalize instead of staying Idle"
        );
        Ok(())
    }

    #[tokio::test]
    async fn missing_tool_execution_context_is_model_visible_and_recoverable() -> TestResult {
        let provider = StubToolProvider::with_names(&["fs.read"]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();
        let model = MissingContextRecoveryModel {
            call_id: ToolCallId::new(),
            responses: AtomicUsize::new(0),
            saw_rejected_tool_result: AtomicBool::new(false),
        };

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("read this"))
            .await
            .map_err(ctx("the model can recover from a rejected tool boundary"))?;

        assert!(matches!(outcome, TurnOutcome::Completed));
        assert_eq!(model.responses.load(Ordering::SeqCst), 2);
        assert!(
            model.saw_rejected_tool_result.load(Ordering::SeqCst),
            "the follow-up model request must include the rejected tool result"
        );
        assert!(matches!(
            session.state(),
            crate::session::SessionState::Idle
        ));
        assert_eq!(session.history().len(), 4);
        Ok(())
    }

    #[tokio::test]
    async fn empty_live_history_is_hydrated_before_a_new_user_turn() -> TestResult {
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();
        let mut persisted = crate::history::ConversationHistory::new();
        persisted.push_user_text("previous question");
        persisted.push_assistant_text("previous answer", None);
        store
            .save_history(session.id(), &persisted)
            .await
            .map_err(ctx("seed history"))?;

        run_turn(
            &mut session,
            &crate::model::EchoModelProvider::new("new answer"),
            &store,
            TurnInput::user("new question"),
        )
        .await
        .map_err(ctx("hydrated turn runs"))?;

        assert_eq!(session.history().len(), 4);
        assert_eq!(store.turn_count(session.id()), 4);
        Ok(())
    }

    #[tokio::test]
    async fn history_load_error_fails_closed_before_user_item_is_admitted() -> TestResult {
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));

        let Err(error) = run_turn(
            &mut session,
            &crate::model::EchoModelProvider::new("must not run"),
            &LoadFailStore,
            TurnInput::user("must not be persisted"),
        )
        .await
        else {
            return Err(TestError::Unexpected(
                "history load failure must reject the turn".to_owned(),
            ));
        };

        assert!(matches!(error, CoreError::TurnRejected(_)));
        assert!(session.history().is_empty());
        assert!(matches!(
            session.state(),
            crate::session::SessionState::Failed(_)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn duplicate_tool_catalog_failure_remains_terminal() -> TestResult {
        let (tx, _rx) = mpsc::unbounded_channel();
        let registry = ExtensionRegistryBuilder::default()
            .tool_provider(StubToolProvider::with_names(&["duplicate"]))
            .tool_provider(StubToolProvider::with_names(&["duplicate"]))
            .build();
        let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx);
        let store = crate::state_store::InMemoryStateStore::new();

        let Err(error) = run_turn(
            &mut session,
            &crate::model::EchoModelProvider::new("must not run"),
            &store,
            TurnInput::user("go"),
        )
        .await
        else {
            return Err(TestError::Unexpected(
                "duplicate tools are an invariant violation".to_owned(),
            ));
        };

        assert!(matches!(error, CoreError::DuplicateTool { .. }));
        assert!(matches!(
            session.state(),
            crate::session::SessionState::Failed(_)
        ));
        Ok(())
    }

    // ------------------------------------------------------------------
    // W2-14: Parallel-Dispatch mit Approval-Vorprüfung
    // ------------------------------------------------------------------

    /// Liefert eine vorprogrammierte Folge von Antworten, eine pro Model-Call.
    struct ScriptedModel {
        responses: Mutex<std::collections::VecDeque<crate::model::ModelResponse>>,
    }

    impl ScriptedModel {
        fn new(responses: Vec<crate::model::ModelResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
            }
        }
    }

    impl ModelProvider for ScriptedModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
            let next = self
                .responses
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front();
            Box::pin(async move { next.ok_or(crate::model::ModelError::EmptyResponse) })
        }
    }

    /// Approval-Handler, der jede Befragung protokolliert und je Call-Id eine
    /// vorprogrammierte Entscheidung liefert. Nicht gelistete Calls → `Allow`.
    struct CountingApproval {
        scripted: Vec<(ToolCallId, ApprovalDecision)>,
        reviews: Mutex<Vec<ToolCallId>>,
    }

    impl CountingApproval {
        fn new(scripted: Vec<(ToolCallId, ApprovalDecision)>) -> Self {
            Self {
                scripted,
                reviews: Mutex::new(Vec::new()),
            }
        }

        fn allow_everything() -> Self {
            Self::new(Vec::new())
        }

        fn reviews_for(&self, call_id: &ToolCallId) -> usize {
            self.reviews
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .filter(|reviewed| *reviewed == call_id)
                .count()
        }

        fn total_reviews(&self) -> usize {
            self.reviews
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len()
        }
    }

    impl harw_extension_api::ApprovalHandler for CountingApproval {
        fn review<'a>(
            &'a self,
            call: &'a ToolCall,
        ) -> harw_extension_api::ExtFuture<'a, ApprovalDecision> {
            self.reviews
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(call.id.clone());
            let decision = self
                .scripted
                .iter()
                .find(|(id, _)| id == &call.id)
                .map_or(ApprovalDecision::Allow, |(_, decision)| decision.clone());
            Box::pin(async move { decision })
        }
    }

    /// Werkzeug, das seine Aufrufe zählt und — mit Barriere — erst zurückkehrt,
    /// wenn die geforderte Zahl an Aufrufen gleichzeitig in Flight ist. Damit
    /// unterscheidet ein Test echte Nebenläufigkeit von serieller Ausführung.
    struct BarrierExecutor {
        executions: Arc<AtomicUsize>,
        barrier: Option<Arc<tokio::sync::Barrier>>,
    }

    impl ToolExecutor for BarrierExecutor {
        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolExecutionContext,
            _call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            let barrier = self.barrier.clone();
            Box::pin(async move {
                if let Some(barrier) = barrier {
                    barrier.wait().await;
                }
                Ok(ToolOutput::Text {
                    content: "ok".to_owned(),
                })
            })
        }
    }

    struct StubParallelProvider {
        names: Vec<ToolName>,
        /// Teilmenge von `names`, die als `parallel_safe` gemeldet wird.
        parallel: Vec<ToolName>,
        executions: Arc<AtomicUsize>,
        barrier: Option<Arc<tokio::sync::Barrier>>,
    }

    impl StubParallelProvider {
        fn instant(names: &[&str], executions: Arc<AtomicUsize>) -> Arc<Self> {
            let names: Vec<ToolName> = names.iter().map(|name| ToolName::new(*name)).collect();
            Arc::new(Self {
                parallel: names.clone(),
                names,
                executions,
                barrier: None,
            })
        }

        fn joined(names: &[&str], executions: Arc<AtomicUsize>, parties: usize) -> Arc<Self> {
            let names: Vec<ToolName> = names.iter().map(|name| ToolName::new(*name)).collect();
            Arc::new(Self {
                parallel: names.clone(),
                names,
                executions,
                barrier: Some(Arc::new(tokio::sync::Barrier::new(parties))),
            })
        }

        fn with_serial_tool(
            names: &[&str],
            serial: &str,
            executions: Arc<AtomicUsize>,
        ) -> Arc<Self> {
            let names: Vec<ToolName> = names.iter().map(|name| ToolName::new(*name)).collect();
            let parallel = names
                .iter()
                .filter(|name| name.as_str() != serial)
                .cloned()
                .collect();
            Arc::new(Self {
                names,
                parallel,
                executions,
                barrier: None,
            })
        }
    }

    impl ToolProvider for StubParallelProvider {
        fn tools(&self) -> Vec<ToolSpec> {
            self.names
                .iter()
                .map(|name| {
                    ToolSpec::Function(FunctionToolSpec {
                        name: name.clone(),
                        description: "stub tool".to_owned(),
                        parameters: JsonSchema::default(),
                        strict: false,
                    })
                })
                .collect()
        }

        fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
            self.names.contains(name).then(|| {
                Arc::new(BarrierExecutor {
                    executions: Arc::clone(&self.executions),
                    barrier: self.barrier.clone(),
                }) as Arc<dyn ToolExecutor>
            })
        }

        fn parallel_safe(&self, name: &ToolName) -> bool {
            self.parallel.contains(name)
        }
    }

    /// Sandbox auf dem echten Harness-Verzeichnis. Ohne aufgelösten
    /// Spawn-Kontext lehnt der Turn-Loop jede Tool-Ausführung ab, der
    /// Parallel-Pfad wäre also gar nicht erreichbar.
    fn test_sandbox() -> TestResult<SandboxSpec> {
        let harness_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or(TestError::Missing(
                "harw-core hat ein Workspace-Elternverzeichnis",
            ))?
            .to_path_buf();
        let registry = harw_authority::WorkspaceRegistry::build(
            &harness_root,
            [harw_authority::WorkspaceRegistration {
                tenant: harw_types::TenantId::from_str("test-tenant"),
                workspace: harw_types::WorkspaceId::from_str("core-parallel-tests"),
                root: std::path::PathBuf::from("harw-core"),
            }],
        )
        .map_err(ctx("Test-Workspace ist registrierbar"))?;
        Ok(SandboxSpec::from_resolved(
            registry
                .resolve(
                    &harw_types::TenantId::from_str("test-tenant"),
                    &harw_types::WorkspaceId::from_str("core-parallel-tests"),
                )
                .map_err(ctx("Test-Workspace löst auf"))?,
            harw_authority::PermissionSet::from_policy([
                harw_authority::Permission::ReadWorkspace,
                harw_authority::Permission::WriteWorkspace,
                harw_authority::Permission::ExecuteProcess,
            ]),
        ))
    }

    fn test_spawn_context() -> TestResult<SpawnContext> {
        Ok(SpawnContext {
            sandbox: test_sandbox()?,
            suggestions: None,
            capability_snapshot: None,
            approval_actor: Some(ApprovalActor::Operator {
                id: "test-operator".to_owned(),
            }),
            organizational_role: harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
            allowed_child_orchestrators: Vec::new(),
            trace: None,
            // `None` reads fail-closed (most restrictive ceiling), not
            // "unbounded" — see `SpawnContext::ceiling`'s own doc. Existing
            // tests that build a session via this helper never exercised a
            // ceiling before this field existed, so `None` keeps their
            // behavior unchanged. Tests that need a real ceiling build their
            // own `SpawnContext` literal instead of this shared helper.
            ceiling: None,
        })
    }

    fn guarded_session(
        provider: Arc<dyn ToolProvider>,
        handler: Arc<CountingApproval>,
    ) -> TestResult<AgentSession> {
        let (tx, _rx) = mpsc::unbounded_channel();
        let registry = ExtensionRegistryBuilder::default()
            .tool_provider(provider)
            .approval_handler(handler)
            .build();
        Ok(AgentSession::new(AgentRole::Assistant, None, registry, tx)
            .with_spawn_context(test_spawn_context()?))
    }

    /// Wie [`guarded_session`], aber mit mehreren Handlern in genau der
    /// übergebenen Registrierungsreihenfolge (W1-05: Aggregation).
    fn multi_guarded_session(
        provider: Arc<dyn ToolProvider>,
        handlers: &[Arc<CountingApproval>],
    ) -> TestResult<AgentSession> {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut builder = ExtensionRegistryBuilder::default().tool_provider(provider);
        for handler in handlers {
            // Erst konkret klonen, dann beim Argument zu `Arc<dyn …>` coercen.
            let handler: Arc<CountingApproval> = Arc::clone(handler);
            builder = builder.approval_handler(handler);
        }
        Ok(
            AgentSession::new(AgentRole::Assistant, None, builder.build(), tx)
                .with_spawn_context(test_spawn_context()?),
        )
    }

    fn call(id: &ToolCallId, name: &str) -> ToolCall {
        ToolCall {
            id: id.clone(),
            name: ToolName::new(name),
            arguments: serde_json::json!({"part": name}),
        }
    }

    fn response_with(calls: Vec<ToolCall>) -> crate::model::ModelResponse {
        crate::model::ModelResponse {
            message: None,
            tool_calls: calls,
            usage: Default::default(),
            ..Default::default()
        }
    }

    fn drain_turn_events(rx: &mut mpsc::UnboundedReceiver<TurnEvent>) -> Vec<TurnEvent> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn a_registered_approval_handler_no_longer_disables_parallel_dispatch() -> TestResult {
        // Regression W2-14: früher genügte ein registrierter Handler, um den
        // JoinSet-Pfad abzuschalten. Die Barriere über zwei Parteien beweist,
        // dass beide Calls tatsächlich gleichzeitig laufen — seriell würde der
        // erste Aufruf nie zurückkehren und der Timeout zuschlagen.
        let first = ToolCallId::new();
        let second = ToolCallId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingApproval::allow_everything());
        let provider = StubParallelProvider::joined(&["lookup"], Arc::clone(&executions), 2);
        let mut session = guarded_session(provider, Arc::clone(&handler))?;
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            response_with(vec![call(&first, "lookup"), call(&second, "lookup")]),
            crate::model::ModelResponse::text("beide Ergebnisse liegen vor"),
        ]);

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_turn(&mut session, &model, &store, TurnInput::user("parallel")),
        )
        .await
        .map_err(ctx(
            "die Calls müssen gleichzeitig laufen, sonst blockiert die Barriere",
        ))?
        .map_err(ctx("der Turn läuft durch"))?;

        assert!(matches!(outcome, TurnOutcome::Completed));
        assert_eq!(executions.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[tokio::test]
    async fn parallel_preflight_asks_every_handler_exactly_once() -> TestResult {
        let first = ToolCallId::new();
        let second = ToolCallId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingApproval::allow_everything());
        let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
        let mut session = guarded_session(provider, Arc::clone(&handler))?;
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            response_with(vec![call(&first, "lookup"), call(&second, "lookup")]),
            crate::model::ModelResponse::text("fertig"),
        ]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("parallel"))
            .await
            .map_err(ctx("der Turn läuft durch"))?;

        assert!(matches!(outcome, TurnOutcome::Completed));
        assert_eq!(executions.load(Ordering::SeqCst), 2);
        assert_eq!(handler.reviews_for(&first), 1);
        assert_eq!(handler.reviews_for(&second), 1);
        assert_eq!(
            handler.total_reviews(),
            2,
            "im Parallel-Pfad darf kein Handler ein zweites Mal gefragt werden"
        );
        Ok(())
    }

    #[tokio::test]
    async fn ask_user_falls_back_to_the_sequential_path_and_pauses_the_turn() -> TestResult {
        let allowed = ToolCallId::new();
        let asked = ToolCallId::new();
        let request = harw_types::ItemId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingApproval::new(vec![(
            asked.clone(),
            ApprovalDecision::AskUser(request.clone()),
        )]));
        let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
        let mut session = guarded_session(provider, Arc::clone(&handler))?;
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![response_with(vec![
            call(&allowed, "lookup"),
            call(&asked, "lookup"),
        ])]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("ask me"))
            .await
            .map_err(ctx("der Turn pausiert statt zu scheitern"))?;

        match &outcome {
            TurnOutcome::AwaitingApproval {
                call_id,
                request: paused_request,
            } => {
                assert_eq!(call_id, &asked);
                assert_eq!(paused_request, &request);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet wurde eine Approval-Pause, nicht {other:?}"
                )));
            }
        }
        assert!(matches!(
            session.state(),
            crate::session::SessionState::WaitingForApproval
        ));
        assert_eq!(
            executions.load(Ordering::SeqCst),
            1,
            "nur der erlaubte Call darf ausgeführt worden sein"
        );
        assert_eq!(handler.reviews_for(&allowed), 1);
        assert_eq!(handler.reviews_for(&asked), 1);
        assert_eq!(
            handler.total_reviews(),
            2,
            "die Vorprüfung reicht ihre Entscheidungen an den sequenziellen \
             Pfad weiter, statt erneut zu fragen"
        );
        Ok(())
    }

    #[tokio::test]
    async fn denied_call_falls_back_to_the_sequential_path_without_asking_twice() -> TestResult {
        let denied = ToolCallId::new();
        let allowed = ToolCallId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingApproval::new(vec![(
            denied.clone(),
            ApprovalDecision::Deny("policy".to_owned()),
        )]));
        let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
        let mut session = guarded_session(provider, Arc::clone(&handler))?;
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            response_with(vec![call(&denied, "lookup"), call(&allowed, "lookup")]),
            crate::model::ModelResponse::text("weiter ohne das verbotene Werkzeug"),
        ]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("deny one"))
            .await
            .map_err(ctx("ein Deny beendet den Turn nicht"))?;

        assert!(matches!(outcome, TurnOutcome::Completed));
        assert_eq!(
            executions.load(Ordering::SeqCst),
            1,
            "der verbotene Call darf nicht ausgeführt werden"
        );
        assert_eq!(handler.reviews_for(&denied), 1);
        assert_eq!(handler.reviews_for(&allowed), 1);
        assert_eq!(handler.total_reviews(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn a_non_parallel_safe_call_skips_the_preflight_entirely() -> TestResult {
        // Die Vorprüfung steht bewusst hinter der Executor-Auflösung: ein
        // Handler darf nicht für einen Parallel-Pfad gefragt werden, der ohnehin
        // verworfen wird.
        let parallel = ToolCallId::new();
        let serial = ToolCallId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingApproval::allow_everything());
        let provider = StubParallelProvider::with_serial_tool(
            &["lookup", "mutate"],
            "mutate",
            Arc::clone(&executions),
        );
        let mut session = guarded_session(provider, Arc::clone(&handler))?;
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            response_with(vec![call(&parallel, "lookup"), call(&serial, "mutate")]),
            crate::model::ModelResponse::text("seriell erledigt"),
        ]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("mixed"))
            .await
            .map_err(ctx("der gemischte Fall läuft seriell durch"))?;

        assert!(matches!(outcome, TurnOutcome::Completed));
        assert_eq!(executions.load(Ordering::SeqCst), 2);
        assert_eq!(handler.reviews_for(&parallel), 1);
        assert_eq!(handler.reviews_for(&serial), 1);
        assert_eq!(handler.total_reviews(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn a_single_call_never_enters_the_parallel_path() -> TestResult {
        let only = ToolCallId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingApproval::allow_everything());
        let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
        let mut session = guarded_session(provider, Arc::clone(&handler))?;
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            response_with(vec![call(&only, "lookup")]),
            crate::model::ModelResponse::text("einzeln erledigt"),
        ]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("single"))
            .await
            .map_err(ctx("ein einzelner Call läuft seriell"))?;

        assert!(matches!(outcome, TurnOutcome::Completed));
        assert_eq!(handler.total_reviews(), 1);
        Ok(())
    }

    // ------------------------------------------------------------------
    // Addendum B: Projektgedächtnis-Erfassung (`ToolOutcomeObserver`)
    // ------------------------------------------------------------------

    /// Zeichnet jedes gemeldete Tool-Ergebnis auf, statt es irgendwo
    /// abzulegen — genügt, um zu prüfen, dass `run_turn` den Beobachter
    /// tatsächlich mit Name und Status erreicht.
    struct RecordingObserver {
        outcomes: Mutex<Vec<(String, crate::capture::ToolOutcomeStatus)>>,
        turns_finished: AtomicUsize,
    }

    impl RecordingObserver {
        fn new() -> Self {
            Self {
                outcomes: Mutex::new(Vec::new()),
                turns_finished: AtomicUsize::new(0),
            }
        }
    }

    impl crate::capture::ToolOutcomeObserver for RecordingObserver {
        fn on_tool_outcome(
            &self,
            _session_id: &harw_types::SessionId,
            outcome: &crate::capture::ToolOutcome<'_>,
        ) {
            self.outcomes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((outcome.tool_name.to_owned(), outcome.status));
        }

        fn on_turn_finished(&self, _session_id: &harw_types::SessionId) {
            self.turns_finished.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn tool_outcome_observer_sees_a_successful_tool_call() -> TestResult {
        let only = ToolCallId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingApproval::allow_everything());
        let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
        let observer = Arc::new(RecordingObserver::new());
        let mut session = guarded_session(provider, Arc::clone(&handler))?
            .with_tool_outcome_observer(Some(
                Arc::clone(&observer) as Arc<dyn crate::capture::ToolOutcomeObserver>
            ));
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            response_with(vec![call(&only, "lookup")]),
            crate::model::ModelResponse::text("erledigt"),
        ]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("single"))
            .await
            .map_err(ctx("ein einzelner Call läuft seriell"))?;

        assert!(matches!(outcome, TurnOutcome::Completed));
        let recorded = observer
            .outcomes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "lookup");
        assert_eq!(recorded[0].1, crate::capture::ToolOutcomeStatus::Success);
        assert_eq!(observer.turns_finished.load(Ordering::SeqCst), 1);
        Ok(())
    }

    // ------------------------------------------------------------------
    // W1-05: Aggregation Deny > AskUser > Allow über alle Handler
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn check_approval_lets_a_later_deny_beat_an_earlier_ask_user() -> TestResult {
        // Regression G-004: früher gewann das vorn registrierte `AskUser`, der
        // Nutzer hätte freigeben können, was eine spätere Politik verbietet.
        let id = ToolCallId::new();
        let asking = Arc::new(CountingApproval::new(vec![(
            id.clone(),
            ApprovalDecision::AskUser(harw_types::ItemId::new()),
        )]));
        let denying = Arc::new(CountingApproval::new(vec![(
            id.clone(),
            ApprovalDecision::Deny("policy".to_owned()),
        )]));
        let session = multi_guarded_session(
            StubToolProvider::with_names(&["lookup"]),
            &[Arc::clone(&asking), Arc::clone(&denying)],
        )?;

        let decision = check_approval(&session, &call(&id, "lookup")).await;

        match decision {
            ApprovalDecision::Deny(reason) => assert_eq!(reason, "policy"),
            other => {
                return Err(TestError::Unexpected(format!(
                    "ein späteres Deny muss gewinnen, war: {other:?}"
                )));
            }
        }
        assert_eq!(asking.reviews_for(&id), 1, "jeder Handler genau einmal");
        assert_eq!(denying.reviews_for(&id), 1, "jeder Handler genau einmal");
        Ok(())
    }

    #[tokio::test]
    async fn check_approval_allows_only_when_every_handler_allows() -> TestResult {
        let id = ToolCallId::new();
        let first = Arc::new(CountingApproval::allow_everything());
        let second = Arc::new(CountingApproval::allow_everything());
        let session = multi_guarded_session(
            StubToolProvider::with_names(&["lookup"]),
            &[Arc::clone(&first), Arc::clone(&second)],
        )?;

        let decision = check_approval(&session, &call(&id, "lookup")).await;

        assert!(matches!(decision, ApprovalDecision::Allow));
        assert_eq!(first.reviews_for(&id), 1);
        assert_eq!(second.reviews_for(&id), 1);
        Ok(())
    }

    #[tokio::test]
    async fn check_approval_keeps_the_first_ask_user_over_allow_and_later_requests() -> TestResult {
        let id = ToolCallId::new();
        let first_request = harw_types::ItemId::new();
        let asking = Arc::new(CountingApproval::new(vec![(
            id.clone(),
            ApprovalDecision::AskUser(first_request.clone()),
        )]));
        let allowing = Arc::new(CountingApproval::allow_everything());
        let asking_later = Arc::new(CountingApproval::new(vec![(
            id.clone(),
            ApprovalDecision::AskUser(harw_types::ItemId::new()),
        )]));
        let session = multi_guarded_session(
            StubToolProvider::with_names(&["lookup"]),
            &[
                Arc::clone(&asking),
                Arc::clone(&allowing),
                Arc::clone(&asking_later),
            ],
        )?;

        let decision = check_approval(&session, &call(&id, "lookup")).await;

        match decision {
            ApprovalDecision::AskUser(request) => assert_eq!(
                request, first_request,
                "die erste Anfrage-ID gilt, spätere werden verworfen"
            ),
            other => {
                return Err(TestError::Unexpected(format!(
                    "AskUser + Allow muss AskUser ergeben, war: {other:?}"
                )));
            }
        }
        assert_eq!(asking.reviews_for(&id), 1);
        assert_eq!(allowing.reviews_for(&id), 1);
        assert_eq!(asking_later.reviews_for(&id), 1);
        Ok(())
    }

    #[tokio::test]
    async fn parallel_preflight_aggregates_a_later_deny_over_an_earlier_ask_user() -> TestResult {
        // Der Parallel-Vorprüfpfad muss identisch aggregieren: statt einer
        // Approval-Pause (früheres `AskUser`) wird der Call abgelehnt, der
        // erlaubte Call läuft, und kein Handler wird doppelt gefragt.
        let allowed = ToolCallId::new();
        let contested = ToolCallId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let asking = Arc::new(CountingApproval::new(vec![(
            contested.clone(),
            ApprovalDecision::AskUser(harw_types::ItemId::new()),
        )]));
        let denying = Arc::new(CountingApproval::new(vec![(
            contested.clone(),
            ApprovalDecision::Deny("policy".to_owned()),
        )]));
        let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
        let mut session =
            multi_guarded_session(provider, &[Arc::clone(&asking), Arc::clone(&denying)])?;
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            response_with(vec![call(&allowed, "lookup"), call(&contested, "lookup")]),
            crate::model::ModelResponse::text("weiter ohne den abgelehnten Call"),
        ]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("aggregate"))
            .await
            .map_err(ctx("ein Deny beendet den Turn nicht"))?;

        assert!(
            matches!(outcome, TurnOutcome::Completed),
            "Deny muss AskUser schlagen, der Turn darf nicht pausieren: {outcome:?}"
        );
        assert_eq!(
            executions.load(Ordering::SeqCst),
            1,
            "nur der erlaubte Call darf ausgeführt werden"
        );
        for handler in [&asking, &denying] {
            assert_eq!(handler.reviews_for(&allowed), 1);
            assert_eq!(handler.reviews_for(&contested), 1);
            assert_eq!(handler.total_reviews(), 2);
        }
        Ok(())
    }

    #[test]
    fn prepared_approvals_yield_each_decision_at_most_once() {
        let mut prepared = PreparedApprovals {
            decisions: vec![Some(ApprovalDecision::Allow), None],
        };

        assert!(matches!(prepared.take(0), Some(ApprovalDecision::Allow)));
        assert!(
            prepared.take(0).is_none(),
            "eine verbrauchte Entscheidung darf nicht erneut geliefert werden"
        );
        assert!(prepared.take(1).is_none());
        assert!(prepared.take(99).is_none(), "ein Index außerhalb ist None");
    }

    // ------------------------------------------------------------------
    // W2-14: Child-Events
    // ------------------------------------------------------------------

    #[test]
    fn child_question_accepts_only_a_string_field() {
        assert_eq!(
            child_question(&serde_json::json!({"question": "welche Datei?"})),
            Some("welche Datei?".to_owned())
        );
        assert_eq!(child_question(&serde_json::json!({"question": 7})), None);
        assert_eq!(
            child_question(&serde_json::json!({"question": {"text": "x"}})),
            None
        );
        assert_eq!(child_question(&serde_json::json!({"task": "x"})), None);
        assert_eq!(child_question(&serde_json::Value::Null), None);
    }

    struct FixedChildSpawner {
        child: SessionId,
    }

    impl AgentSpawner for FixedChildSpawner {
        fn spawn_child<'a>(
            &'a self,
            _role: &'a str,
            _input: SpawnInput,
            _sandbox: SandboxSpec,
            _suggestions: Option<AgentSuggestions>,
        ) -> SpawnFuture<'a> {
            let child = self.child.clone();
            Box::pin(async move { Ok(child) })
        }
    }

    #[tokio::test]
    async fn handoff_emits_child_spawned_with_role_and_question() -> TestResult {
        let child = SessionId::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
        let registry = ExtensionRegistryBuilder::default()
            .spawner(Arc::new(FixedChildSpawner {
                child: child.clone(),
            }))
            .build();
        let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
            .with_spawn_context(test_spawn_context()?)
            .with_turn_event_sink(turn_tx);
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("transfer_to_worker"),
            arguments: serde_json::json!({"question": "wo liegt der Fehler?"}),
        }])]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("handoff"))
            .await
            .map_err(ctx("der Handoff pausiert den Turn"))?;

        assert!(matches!(outcome, TurnOutcome::AwaitingChild { .. }));
        let spawned = drain_turn_events(&mut turn_rx)
            .into_iter()
            .find(|event| matches!(event, TurnEvent::ChildSpawned { .. }))
            .ok_or(TestError::Missing("ein Handoff meldet ein ChildSpawned"))?;
        match spawned {
            TurnEvent::ChildSpawned {
                child: spawned_child,
                role,
                question,
                ..
            } => {
                assert_eq!(spawned_child, child);
                assert_eq!(role, "worker");
                assert_eq!(question.as_deref(), Some("wo liegt der Fehler?"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet wurde ChildSpawned, nicht {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn handoff_without_a_question_argument_reports_none() -> TestResult {
        let child = SessionId::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
        let registry = ExtensionRegistryBuilder::default()
            .spawner(Arc::new(FixedChildSpawner {
                child: child.clone(),
            }))
            .build();
        let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
            .with_spawn_context(test_spawn_context()?)
            .with_turn_event_sink(turn_tx);
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("transfer_to_reviewer"),
            arguments: serde_json::json!({"task": "review"}),
        }])]);

        run_turn(&mut session, &model, &store, TurnInput::user("handoff"))
            .await
            .map_err(ctx("der Handoff pausiert den Turn"))?;

        let spawned = drain_turn_events(&mut turn_rx)
            .into_iter()
            .find(|event| matches!(event, TurnEvent::ChildSpawned { .. }))
            .ok_or(TestError::Missing("ein Handoff meldet ein ChildSpawned"))?;
        assert!(matches!(
            spawned,
            TurnEvent::ChildSpawned { question: None, role, .. } if role == "reviewer"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn child_completion_emits_child_completed_after_the_result_is_persisted() -> TestResult {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
        let child = SessionId::new();
        let registry = ExtensionRegistryBuilder::default()
            .spawner(Arc::new(FixedChildSpawner {
                child: child.clone(),
            }))
            .build();
        let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
            .with_spawn_context(test_spawn_context()?)
            .with_turn_event_sink(turn_tx);
        session.try_start_turn().map_err(ctx("Turn startet"))?;
        let call_id = ToolCallId::new();
        session
            .begin_handoff(child.clone(), call_id.clone(), "worker".to_owned())
            .map_err(ctx("Handoff pausiert"))?;
        let store = crate::state_store::InMemoryStateStore::new();

        let outcome = resume_after_child(
            &mut session,
            &crate::model::EchoModelProvider::new("Eltern-Turn läuft weiter"),
            &store,
            child.clone(),
            call_id,
            ToolCallResult::success(serde_json::json!({"child": "done"})),
        )
        .await
        .map_err(ctx("das Kind-Ergebnis nimmt den Eltern-Turn wieder auf"))?;

        assert!(matches!(outcome, TurnOutcome::Completed));
        let completed = drain_turn_events(&mut turn_rx)
            .into_iter()
            .find(|event| matches!(event, TurnEvent::ChildCompleted { .. }))
            .ok_or(TestError::Missing(
                "ein terminiertes Kind meldet ChildCompleted",
            ))?;
        match completed {
            TurnEvent::ChildCompleted {
                child: completed_child,
                outcome,
                duration_ms,
                ..
            } => {
                assert_eq!(completed_child, child);
                assert_eq!(outcome, "completed");
                assert_eq!(
                    duration_ms, 0,
                    "die Laufzeit des Kindes ist an dieser Stelle nicht bekannt \
                     und wird deshalb nicht geraten"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet wurde ChildCompleted, nicht {other:?}"
                )));
            }
        }
        Ok(())
    }

    // Keep the unused import lint quiet — ToolsError is used in the
    // ToolExecutorFuture return type via the type alias.
    #[allow(dead_code)]
    fn _assert_tools_error_used(_: ToolsError) {}

    // ------------------------------------------------------------------
    // TurnControl wiring (Moduldoku „Fünfter Nachtrag"): ein gerissenes
    // Budget endet als `Cancelled { reason: Budget }`, ein Turn innerhalb
    // der Grenzen bleibt `Completed`.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn turn_exceeding_its_model_round_budget_is_cancelled_with_budget_reason() -> TestResult {
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();
        // `max_model_rounds: 0` reißt bereits am allerersten Prüfpunkt, vor
        // jedem Modellaufruf — der Provider darf deshalb nie befragt werden.
        let control = TurnControl::new().with_limits(TurnLimits {
            max_model_rounds: 0,
            ..TurnLimits::unlimited()
        });
        let input = TurnInput::user("hello").with_control(control);

        let outcome = run_turn(
            &mut session,
            &crate::model::EchoModelProvider::new("must never be called"),
            &store,
            input,
        )
        .await
        .map_err(ctx("a tripped budget ends the turn cleanly, not as an Err"))?;

        assert!(
            matches!(
                outcome,
                TurnOutcome::Cancelled {
                    reason: CancelReason::Budget
                }
            ),
            "expected Cancelled{{reason: Budget}}, got {outcome:?}"
        );
        assert!(
            matches!(session.state(), crate::session::SessionState::Idle),
            "a cancelled turn returns the session to Idle, exactly like Completed"
        );
        Ok(())
    }

    #[tokio::test]
    async fn turn_within_its_limits_still_completes() -> TestResult {
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();
        let control = TurnControl::new().with_limits(TurnLimits {
            max_model_rounds: 4,
            max_tool_calls: 4,
            max_output_tokens_total: 1_000,
            wall_time: Duration::from_secs(30),
            tool_result_max_bytes: 4_096,
        });
        // Ein Klon, um nach dem Turn zu prüfen, dass der Steuerblock die
        // Modellrunde tatsächlich mitgezählt hat (Moduldoku „Fünfter
        // Nachtrag") — `input` verbraucht das Original.
        let control_check = control.clone();
        let input = TurnInput::user("hello").with_control(control);

        let outcome = run_turn(
            &mut session,
            &crate::model::EchoModelProvider::new("hi there"),
            &store,
            input,
        )
        .await
        .map_err(ctx("a turn within its budget completes normally"))?;

        assert!(
            matches!(outcome, TurnOutcome::Completed),
            "expected Completed, got {outcome:?}"
        );
        assert!(
            matches!(session.state(), crate::session::SessionState::Idle),
            "a completed turn returns the session to Idle"
        );
        assert_eq!(
            control_check.model_rounds(),
            1,
            "drive_turn must record the one model round it actually ran"
        );
        Ok(())
    }

    // ------------------------------------------------------------------
    // Cancel mid-flight (Moduldoku „Sechster Nachtrag"): der Modellaufruf
    // selbst und eine laufende Werkzeugausführung racen jetzt gegen
    // `control.cancel_token()` statt nur an den Prüfpunkten davor/danach.
    // ------------------------------------------------------------------

    /// Modell, dessen `respond()` erst nach `delay` antwortet — lang genug,
    /// dass ein Test ohne funktionierende Racing-Verdrahtung an seinem
    /// `tokio::time::timeout` scheitert statt fälschlich grün zu werden.
    /// `started` feuert, sobald `respond()` betreten wurde, damit der Test
    /// genau dann abbrechen kann, wenn der Aufruf wirklich in Flight ist.
    struct DelayedModelProvider {
        delay: Duration,
        started: Arc<tokio::sync::Notify>,
    }

    impl ModelProvider for DelayedModelProvider {
        fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
            self.started.notify_one();
            let delay = self.delay;
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                Ok(crate::model::ModelResponse::text(
                    "too late — cancellation should have won",
                ))
            })
        }
    }

    /// Ausführer, der erst zurückkehrt, wenn sein
    /// [`ToolExecutionContext::cancel`]-Token feuert — dann mit
    /// `ToolsError::Cancelled`, genau wie ein wohlerzogener, cancel-bewusster
    /// Ausführer (W3/C-CANCEL) reagieren muss. Fehlt die Cancel-Verdrahtung
    /// (`ctx.cancel()` liefert `None`), schläft er stattdessen lange genug,
    /// dass der Test am `tokio::time::timeout` scheitert statt zu hängen.
    /// `started` feuert bei jedem Aufruf, damit ein Test exakt weiß, wann
    /// mindestens ein Aufruf tatsächlich läuft.
    struct CancelAwareExecutor {
        executions: Arc<AtomicUsize>,
        started: Arc<tokio::sync::Notify>,
    }

    impl ToolExecutor for CancelAwareExecutor {
        fn execute<'a>(
            &'a self,
            ctx: &'a ToolExecutionContext,
            _call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            self.started.notify_one();
            Box::pin(async move {
                match ctx.cancel() {
                    Some(token) => {
                        token.cancelled().await;
                        Err(ToolsError::Cancelled)
                    }
                    None => {
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        Ok(ToolOutput::Text {
                            content: "unreachable without cancel wiring".to_owned(),
                        })
                    }
                }
            })
        }
    }

    /// Einzelnes, **nicht** `parallel_safe` markiertes Werkzeug `"block"` —
    /// erzwingt den sequenziellen Dispatch-Pfad.
    struct CancelAwareToolProvider {
        executions: Arc<AtomicUsize>,
        started: Arc<tokio::sync::Notify>,
    }

    impl ToolProvider for CancelAwareToolProvider {
        fn tools(&self) -> Vec<ToolSpec> {
            vec![ToolSpec::Function(FunctionToolSpec {
                name: ToolName::new("block"),
                description: "blocks until its cancel token fires".to_owned(),
                parameters: JsonSchema::default(),
                strict: false,
            })]
        }

        fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
            (name.as_str() == "block").then(|| {
                Arc::new(CancelAwareExecutor {
                    executions: Arc::clone(&self.executions),
                    started: Arc::clone(&self.started),
                }) as Arc<dyn ToolExecutor>
            })
        }
    }

    /// Wie [`CancelAwareToolProvider`], aber `"block"` ist `parallel_safe` —
    /// erzwingt den `JoinSet`-Pfad ([`try_execute_parallel_calls`]).
    struct CancelAwareParallelProvider {
        executions: Arc<AtomicUsize>,
        started: Arc<tokio::sync::Notify>,
    }

    impl ToolProvider for CancelAwareParallelProvider {
        fn tools(&self) -> Vec<ToolSpec> {
            vec![ToolSpec::Function(FunctionToolSpec {
                name: ToolName::new("block"),
                description: "blocks until its cancel token fires".to_owned(),
                parameters: JsonSchema::default(),
                strict: false,
            })]
        }

        fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
            (name.as_str() == "block").then(|| {
                Arc::new(CancelAwareExecutor {
                    executions: Arc::clone(&self.executions),
                    started: Arc::clone(&self.started),
                }) as Arc<dyn ToolExecutor>
            })
        }

        fn parallel_safe(&self, name: &ToolName) -> bool {
            name.as_str() == "block"
        }
    }

    #[tokio::test]
    async fn model_call_cancelled_mid_flight_ends_the_turn_quickly() -> TestResult {
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();
        let started = Arc::new(tokio::sync::Notify::new());
        let model = DelayedModelProvider {
            delay: Duration::from_secs(30),
            started: Arc::clone(&started),
        };
        let control = TurnControl::new();
        let cancel_token = control.cancel_token().clone();
        let input = TurnInput::user("hello").with_control(control);

        let run = run_turn(&mut session, &model, &store, input);
        let canceller = async {
            started.notified().await;
            cancel_token.cancel(CancelReason::User);
        };

        let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(run, canceller)
        })
        .await
        .map_err(ctx(
            "cancellation must win long before the provider's 30s delay elapses — \
             the model call is not actually racing the cancel token",
        ))?;
        let outcome = outcome.map_err(ctx(
            "a cancelled model call ends the turn cleanly, not as Err",
        ))?;

        assert!(
            matches!(
                outcome,
                TurnOutcome::Cancelled {
                    reason: CancelReason::User
                }
            ),
            "expected Cancelled{{reason: User}}, got {outcome:?}"
        );
        assert!(
            matches!(session.state(), crate::session::SessionState::Idle),
            "a cancelled turn returns the session to Idle, exactly like Completed"
        );
        Ok(())
    }

    #[tokio::test]
    async fn tool_call_cancelled_mid_flight_aborts_pending_calls_sequential_path() -> TestResult {
        let first = ToolCallId::new();
        let second = ToolCallId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let provider = Arc::new(CancelAwareToolProvider {
            executions: Arc::clone(&executions),
            started: Arc::clone(&started),
        });
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
            .with_spawn_context(test_spawn_context()?);
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![response_with(vec![
            call(&first, "block"),
            call(&second, "block"),
        ])]);
        let control = TurnControl::new();
        let cancel_token = control.cancel_token().clone();
        let input = TurnInput::user("go").with_control(control);

        let run = run_turn(&mut session, &model, &store, input);
        let canceller = async {
            started.notified().await;
            cancel_token.cancel(CancelReason::User);
        };

        let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(run, canceller)
        })
        .await
        .map_err(ctx(
            "cancellation must resolve the blocked sequential call quickly",
        ))?;
        let outcome = outcome.map_err(ctx(
            "a cancelled sequential tool call ends the turn cleanly, not as Err",
        ))?;

        assert!(
            matches!(
                outcome,
                TurnOutcome::Cancelled {
                    reason: CancelReason::User
                }
            ),
            "expected Cancelled{{reason: User}}, got {outcome:?}"
        );
        assert_eq!(
            executions.load(Ordering::SeqCst),
            1,
            "the second call must never start — it was still pending in `tool_call_iter` \
             when cancellation hit, exactly like a `tool_checkpoint()` pre-emptive hit"
        );
        let items = session.history().items();
        assert_eq!(
            items
                .iter()
                .filter(|item| matches!(item, TurnItem::ToolCall(_)))
                .count(),
            2,
            "both calls must have a tool_call entry (the second one synthesized by \
             cancel_turn_with_pending_calls)"
        );
        assert_eq!(
            items
                .iter()
                .filter(|item| matches!(item, TurnItem::ToolResult(_)))
                .count(),
            2,
            "every tool_call must be paired with a tool_result, or the history is not \
             provider-valid for the next model call"
        );
        Ok(())
    }

    #[tokio::test]
    async fn tool_call_cancelled_mid_flight_aborts_pending_calls_parallel_path() -> TestResult {
        let first = ToolCallId::new();
        let second = ToolCallId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(tokio::sync::Notify::new());
        let handler = Arc::new(CountingApproval::allow_everything());
        let provider = Arc::new(CancelAwareParallelProvider {
            executions: Arc::clone(&executions),
            started: Arc::clone(&started),
        });
        let mut session = guarded_session(provider, Arc::clone(&handler))?;
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![response_with(vec![
            call(&first, "block"),
            call(&second, "block"),
        ])]);
        let control = TurnControl::new();
        let cancel_token = control.cancel_token().clone();
        let input = TurnInput::user("go").with_control(control);

        let run = run_turn(&mut session, &model, &store, input);
        let canceller = async {
            started.notified().await;
            cancel_token.cancel(CancelReason::User);
        };

        let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(run, canceller)
        })
        .await
        .map_err(ctx(
            "cancellation must resolve the blocked parallel calls quickly",
        ))?;
        let outcome = outcome.map_err(ctx(
            "a cancelled parallel tool call ends the turn cleanly, not as Err",
        ))?;

        assert!(
            matches!(
                outcome,
                TurnOutcome::Cancelled {
                    reason: CancelReason::User
                }
            ),
            "expected Cancelled{{reason: User}}, got {outcome:?}"
        );
        let items = session.history().items();
        assert_eq!(
            items
                .iter()
                .filter(|item| matches!(item, TurnItem::ToolCall(_)))
                .count(),
            2,
            "the JoinSet path pushes both tool_call entries up front, before spawning"
        );
        let results: Vec<ToolCallResult> = items
            .iter()
            .filter_map(|item| match item {
                TurnItem::ToolResult(result) => Some(result.result.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            results.len(),
            2,
            "every tool_call must be paired with a tool_result, or the history is not \
             provider-valid for the next model call"
        );
        // Angepasst für Fix C (Moduldoku „Siebter Nachtrag"): vor Fix C konnte
        // dieser Test einen deterministischen 1-und-1-Split annehmen — ein
        // Call lieferte über `CancelAwareExecutor`s eigenes
        // `Err(ToolsError::Cancelled)` aus, der andere war beim `TurnGuard`-
        // artigen Abort noch unzugestellt. Fix C lässt die Reaper-Schleife
        // selbst gegen `control.cancel_token()` racen und `joins.abort_all()`
        // rufen, sobald sie gewinnt — das kann jetzt auch einen an sich
        // Cancel-bewussten Job abbrechen, bevor er seine eigene
        // `Err(ToolsError::Cancelled)`-Antwort zurückgeben konnte (ein
        // `abort()`ter Task wird nie zu Ende poll't). Welcher der beiden Wege
        // im Einzelfall gewinnt, ist jetzt eine echte, gewollte
        // Scheduler-Race — der Test prüft deshalb nur noch die Invariante,
        // die für beide Fälle gilt: jedes Ergebnis ist ein
        // Cancel-Fehlerergebnis, keines ein generischer Tool-Fehler.
        let all_cancelled = results.iter().all(|result| {
            matches!(
                result,
                ToolCallResult::Error { message }
                    if message == "tool execution was cancelled"
                        || message.contains("cancelled before delivery")
            )
        });
        assert!(
            all_cancelled,
            "every result must be cancellation-shaped — either the executor's own \
             ToolsError::Cancelled message or the synthetic 'cancelled before delivery' \
             fill-in — got {results:?}"
        );
        Ok(())
    }

    // ------------------------------------------------------------------
    // Fix A/B/C (Moduldoku „Siebter Nachtrag"): der Steuerblock übersteht
    // jetzt eine Handoff-/Rückfrage-Pause, und zwei bisher ungeracete
    // Ausführungspfade (Rückfrage-Fortsetzung sequenziell; paralleler
    // Join-Loop) racen jetzt auch gegen `control.cancel_token()`.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn cancelling_the_original_control_after_a_handoff_pause_cancels_the_resumed_turn()
    -> TestResult {
        let child = SessionId::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        let registry = ExtensionRegistryBuilder::default()
            .spawner(Arc::new(FixedChildSpawner {
                child: child.clone(),
            }))
            .build();
        let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
            .with_spawn_context(test_spawn_context()?);
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("transfer_to_worker"),
            arguments: serde_json::json!({}),
        }])]);
        let control = TurnControl::new();
        let cancel_token = control.cancel_token().clone();
        let input = TurnInput::user("go").with_control(control);

        let outcome = run_turn(&mut session, &model, &store, input)
            .await
            .map_err(ctx("the handoff pauses the turn"))?;
        let (child_returned, call_id_returned) = match outcome {
            TurnOutcome::AwaitingChild {
                child: awaited_child,
                call_id: awaited_call_id,
                ..
            } => (awaited_child, awaited_call_id),
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected AwaitingChild, got {other:?}"
                )));
            }
        };

        // Ctrl+C-Äquivalent: der URSPRÜNGLICHE Token wird erst NACH der Pause
        // abgebrochen. Vor Fix A verschluckte `resume_after_child_with_approvals`s
        // frischer, unbegrenzter `TurnControl::new()` genau diesen Fall — dieser
        // Test muss vor Fix A rot sein (Ergebnis `Completed`), danach grün
        // (Ergebnis `Cancelled`).
        cancel_token.cancel(CancelReason::User);

        let outcome = resume_after_child(
            &mut session,
            &crate::model::EchoModelProvider::new("must not be reached"),
            &store,
            child_returned,
            call_id_returned,
            ToolCallResult::success(serde_json::json!({"child": "done"})),
        )
        .await
        .map_err(ctx(
            "a cancelled resume still ends the turn cleanly, not as Err",
        ))?;

        assert!(
            matches!(
                outcome,
                TurnOutcome::Cancelled {
                    reason: CancelReason::User
                }
            ),
            "AgentSession::active_turn_control must carry the original control across the \
             handoff pause so the resumed turn observes its cancellation — got {outcome:?}"
        );
        assert!(
            matches!(session.state(), crate::session::SessionState::Idle),
            "a cancelled turn returns the session to Idle, exactly like Completed"
        );
        Ok(())
    }

    /// Ausführer, der `ctx.cancel()` NIE selbst abfragt — schläft `delay`
    /// lang und liefert danach ein gewöhnliches Ergebnis. Anders als
    /// [`CancelAwareExecutor`] oben: dieser Fixture prüft, dass Fix B/C auch
    /// einen unkooperativen Ausführer unterbrechen, nicht nur einen, der den
    /// Token selbst abfragt.
    struct DelayedNonCancelAwareExecutor {
        delay: Duration,
        started: Arc<tokio::sync::Notify>,
    }

    impl ToolExecutor for DelayedNonCancelAwareExecutor {
        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolExecutionContext,
            _call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            self.started.notify_one();
            let delay = self.delay;
            Box::pin(async move {
                tokio::time::sleep(delay).await;
                Ok(ToolOutput::Text {
                    content: "too late — the outer cancel race should have won".to_owned(),
                })
            })
        }
    }

    /// Einzelnes, **nicht** `parallel_safe` markiertes Werkzeug `"slow"` —
    /// erzwingt den sequenziellen Dispatch-Pfad.
    struct SlowToolProvider {
        started: Arc<tokio::sync::Notify>,
    }

    impl ToolProvider for SlowToolProvider {
        fn tools(&self) -> Vec<ToolSpec> {
            vec![ToolSpec::Function(FunctionToolSpec {
                name: ToolName::new("slow"),
                description: "ignores cancellation and sleeps".to_owned(),
                parameters: JsonSchema::default(),
                strict: false,
            })]
        }

        fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
            (name.as_str() == "slow").then(|| {
                Arc::new(DelayedNonCancelAwareExecutor {
                    delay: Duration::from_secs(30),
                    started: Arc::clone(&self.started),
                }) as Arc<dyn ToolExecutor>
            })
        }
    }

    /// Wie [`SlowToolProvider`], aber `"slow"` ist `parallel_safe` — erzwingt
    /// den `JoinSet`-Pfad ([`try_execute_parallel_calls`]).
    struct SlowParallelToolProvider {
        started: Arc<tokio::sync::Notify>,
    }

    impl ToolProvider for SlowParallelToolProvider {
        fn tools(&self) -> Vec<ToolSpec> {
            vec![ToolSpec::Function(FunctionToolSpec {
                name: ToolName::new("slow"),
                description: "ignores cancellation and sleeps".to_owned(),
                parameters: JsonSchema::default(),
                strict: false,
            })]
        }

        fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
            (name.as_str() == "slow").then(|| {
                Arc::new(DelayedNonCancelAwareExecutor {
                    delay: Duration::from_secs(30),
                    started: Arc::clone(&self.started),
                }) as Arc<dyn ToolExecutor>
            })
        }

        fn parallel_safe(&self, name: &ToolName) -> bool {
            name.as_str() == "slow"
        }
    }

    #[tokio::test]
    async fn tool_call_ignoring_cancel_itself_is_still_interrupted_sequential_path() -> TestResult {
        let call_id = ToolCallId::new();
        let started = Arc::new(tokio::sync::Notify::new());
        let provider = Arc::new(SlowToolProvider {
            started: Arc::clone(&started),
        });
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
            .with_spawn_context(test_spawn_context()?);
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![response_with(vec![call(&call_id, "slow")])]);
        let control = TurnControl::new();
        let cancel_token = control.cancel_token().clone();
        let input = TurnInput::user("go").with_control(control);

        let run = run_turn(&mut session, &model, &store, input);
        let canceller = async {
            started.notified().await;
            cancel_token.cancel(CancelReason::User);
        };

        let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(run, canceller)
        })
        .await
        .map_err(ctx(
            "Fix B must race the executor's own await against the cancel token — an \
             executor that never checks cancellation itself must not be able to block the \
             turn for its full 30s delay",
        ))?;
        let outcome = outcome.map_err(ctx(
            "a cancelled tool call ends the turn cleanly, not as Err",
        ))?;

        assert!(
            matches!(
                outcome,
                TurnOutcome::Cancelled {
                    reason: CancelReason::User
                }
            ),
            "expected Cancelled{{reason: User}}, got {outcome:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn tool_call_ignoring_cancel_itself_is_still_interrupted_ask_user_resume_path()
    -> TestResult {
        let call_id = ToolCallId::new();
        let started = Arc::new(tokio::sync::Notify::new());
        let provider = Arc::new(SlowToolProvider {
            started: Arc::clone(&started),
        });
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
            .with_spawn_context(test_spawn_context()?);
        session.try_start_turn().map_err(ctx("test turn starts"))?;
        let actor = ApprovalActor::Operator {
            id: "test-operator".to_owned(),
        };
        let pending_call = ToolCall {
            id: call_id.clone(),
            name: ToolName::new("slow"),
            arguments: serde_json::json!({}),
        };
        session
            .begin_approval(
                pending_call,
                harw_types::ItemId::new(),
                actor.clone(),
                jiff::Timestamp::now(),
            )
            .map_err(ctx("pause for approval"))?;
        // Fix B testet den Rückfrage-Fortsetzungspfad unabhängig von Fix A:
        // der Steuerblock wird hier direkt über den neuen Setter hinterlegt,
        // statt über einen vollen `run_turn`-Durchlauf.
        let control = TurnControl::new();
        let cancel_token = control.cancel_token().clone();
        session.set_active_turn_control(control);
        let store = crate::state_store::InMemoryStateStore::new();
        let model = crate::model::EchoModelProvider::new("must not be reached");

        let run = resume_after_approval(
            &mut session,
            &model,
            &store,
            actor,
            ApprovalResolution::Approve,
        );
        let canceller = async {
            started.notified().await;
            cancel_token.cancel(CancelReason::User);
        };

        let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(run, canceller)
        })
        .await
        .map_err(ctx(
            "Fix B must race the resume-after-approval executor call against the cancel \
             token too — this path had no was_cancelled handling at all before this node",
        ))?;
        let outcome = outcome.map_err(ctx(
            "a cancelled resumed tool call ends the turn cleanly, not as Err",
        ))?;

        assert!(
            matches!(
                outcome,
                TurnOutcome::Cancelled {
                    reason: CancelReason::User
                }
            ),
            "expected Cancelled{{reason: User}}, got {outcome:?}"
        );
        Ok(())
    }

    // Fix A (Moduldoku „Achter Nachtrag"): a transport error on the model
    // call that `drive_turn` makes right after a resumed approval's tool
    // executes must not leave the session stuck in `Running` forever.
    // Before the fix, every error exit of `resume_after_approval_with_store`
    // after `resolve_approval` succeeded (here: the trailing `drive_turn`
    // call) returned directly, bypassing `transition_after_turn_failure` —
    // the next `try_start_turn` then failed with "session not idle:
    // Running" even though the failure itself was retryable.
    #[tokio::test]
    async fn resume_after_approval_retryable_provider_failure_leaves_session_idle_not_running()
    -> TestResult {
        let call_id = ToolCallId::new();
        let provider = StubToolProvider::with_names(&["fs.read"]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
            .with_spawn_context(test_spawn_context()?);
        session.try_start_turn().map_err(ctx("test turn starts"))?;
        let actor = ApprovalActor::Operator {
            id: "test-operator".to_owned(),
        };
        let pending_call = ToolCall {
            id: call_id.clone(),
            name: ToolName::new("fs.read"),
            arguments: serde_json::json!({}),
        };
        session
            .begin_approval(
                pending_call,
                harw_types::ItemId::new(),
                actor.clone(),
                jiff::Timestamp::now(),
            )
            .map_err(ctx("pause for approval"))?;
        session.set_active_turn_control(TurnControl::new());
        let store = crate::state_store::InMemoryStateStore::new();

        let Err(error) = resume_after_approval(
            &mut session,
            &TransientModel,
            &store,
            actor,
            ApprovalResolution::Approve,
        )
        .await
        else {
            return Err(TestError::Unexpected(
                "the transport error surfaces to the caller".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            CoreError::Model(crate::model::ModelError::Transient { .. })
        ));
        assert!(
            matches!(session.state(), crate::session::SessionState::Idle),
            "a retryable provider failure on the post-approval drive_turn call must not leave \
             the session stuck in Running, got {:?}",
            session.state()
        );

        let outcome = run_turn(
            &mut session,
            &crate::model::EchoModelProvider::new("recovered"),
            &store,
            TurnInput::user("try again"),
        )
        .await
        .map_err(ctx(
            "the same session accepts a new turn after the retryable failure",
        ))?;
        assert!(matches!(outcome, TurnOutcome::Completed));
        Ok(())
    }

    // Companion to the retryable case above: a non-retryable provider
    // failure on the post-approval `drive_turn` call must still move the
    // session to the terminal `Failed` state via `transition_after_turn_failure`
    // — never leave it hanging in `Running`.
    #[tokio::test]
    async fn resume_after_approval_non_retryable_provider_failure_terminalizes_session()
    -> TestResult {
        let call_id = ToolCallId::new();
        let provider = StubToolProvider::with_names(&["fs.read"]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
            .with_spawn_context(test_spawn_context()?);
        session.try_start_turn().map_err(ctx("test turn starts"))?;
        let actor = ApprovalActor::Operator {
            id: "test-operator".to_owned(),
        };
        let pending_call = ToolCall {
            id: call_id.clone(),
            name: ToolName::new("fs.read"),
            arguments: serde_json::json!({}),
        };
        session
            .begin_approval(
                pending_call,
                harw_types::ItemId::new(),
                actor.clone(),
                jiff::Timestamp::now(),
            )
            .map_err(ctx("pause for approval"))?;
        session.set_active_turn_control(TurnControl::new());
        let store = crate::state_store::InMemoryStateStore::new();

        let Err(error) = resume_after_approval(
            &mut session,
            &RefusalModel,
            &store,
            actor,
            ApprovalResolution::Approve,
        )
        .await
        else {
            return Err(TestError::Unexpected(
                "the refusal surfaces to the caller".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            CoreError::Model(crate::model::ModelError::Refusal { .. })
        ));
        assert!(
            matches!(session.state(), crate::session::SessionState::Failed(_)),
            "a non-retryable provider failure on the post-approval drive_turn call must \
             terminalize the session instead of leaving it Running, got {:?}",
            session.state()
        );
        Ok(())
    }

    #[tokio::test]
    async fn parallel_dispatch_aborts_still_running_non_cancel_aware_calls_on_cancel() -> TestResult
    {
        let first = ToolCallId::new();
        let second = ToolCallId::new();
        let started = Arc::new(tokio::sync::Notify::new());
        let handler = Arc::new(CountingApproval::allow_everything());
        let provider = Arc::new(SlowParallelToolProvider {
            started: Arc::clone(&started),
        });
        let mut session = guarded_session(provider, Arc::clone(&handler))?;
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![response_with(vec![
            call(&first, "slow"),
            call(&second, "slow"),
        ])]);
        let control = TurnControl::new();
        let cancel_token = control.cancel_token().clone();
        let input = TurnInput::user("go").with_control(control);

        let run = run_turn(&mut session, &model, &store, input);
        let canceller = async {
            started.notified().await;
            cancel_token.cancel(CancelReason::User);
        };

        let (outcome, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(run, canceller)
        })
        .await
        .map_err(ctx(
            "Fix C must race the JoinSet reaper loop against the cancel token and \
             abort_all() still-running jobs — executors that never check cancellation \
             themselves must not be able to block the turn for their full 30s delay",
        ))?;
        let outcome = outcome.map_err(ctx(
            "a cancelled parallel dispatch ends the turn cleanly, not as Err",
        ))?;

        assert!(
            matches!(
                outcome,
                TurnOutcome::Cancelled {
                    reason: CancelReason::User
                }
            ),
            "expected Cancelled{{reason: User}}, got {outcome:?}"
        );
        let items = session.history().items();
        assert_eq!(
            items
                .iter()
                .filter(|item| matches!(item, TurnItem::ToolCall(_)))
                .count(),
            2,
            "the JoinSet path pushes both tool_call entries up front, before spawning"
        );
        let results: Vec<ToolCallResult> = items
            .iter()
            .filter_map(|item| match item {
                TurnItem::ToolResult(result) => Some(result.result.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            results.len(),
            2,
            "every tool_call must be paired with a tool_result — including the calls that \
             were still in flight when abort_all() fired, which the synthetic fill-in \
             (Fix C) must cover"
        );
        assert!(
            results.iter().all(|result| matches!(
                result,
                ToolCallResult::Error { message } if message.contains("cancelled before delivery")
            )),
            "neither call checks cancellation itself, so abort_all() must be the only way \
             either of them ever resolves, and both must get the synthetic 'cancelled \
             before delivery' fill-in — got {results:?}"
        );
        Ok(())
    }

    // ------------------------------------------------------------------
    // maybe_compact tests
    // ------------------------------------------------------------------

    /// Sammelt den letzten `CompactionOutcome`, den `maybe_compact` über
    /// `compact_session` erzeugt hat — Test-Double für
    /// [`crate::compaction::CompactionObserver`].
    struct RecordingCompactionObserver {
        last: Mutex<Option<crate::compaction::CompactionOutcome>>,
    }

    impl RecordingCompactionObserver {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                last: Mutex::new(None),
            })
        }
    }

    impl crate::compaction::CompactionObserver for RecordingCompactionObserver {
        fn on_compacted(
            &self,
            _session_id: &SessionId,
            outcome: &crate::compaction::CompactionOutcome,
        ) {
            *self
                .last
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome.clone());
        }
    }

    #[tokio::test]
    async fn test_maybe_compact_noop_without_policy() {
        // `AgentSession::auto_compact` ist standardmäßig `None` — bestehende
        // Sessions, die nie opt-in, dürfen von `maybe_compact` nicht berührt
        // werden.
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        session.history_mut().push_user_text("hallo");
        let observer = RecordingCompactionObserver::new();
        session = session.with_compaction_observer(Some(observer.clone()));
        let store = crate::state_store::InMemoryStateStore::new();
        let usage = harw_types::TokenUsage {
            cache_separate: false,
            input_tokens: u64::MAX,
            output_tokens: 0,
            reasoning_tokens: None,
            cached_tokens: None,
            cache_write_tokens: None,
        };

        maybe_compact(
            &mut session,
            &crate::model::EchoModelProvider::default(),
            &store,
            &usage,
            true,
        )
        .await;

        assert!(
            observer
                .last
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none(),
            "no policy set ⇒ compact_session must never run"
        );
    }

    #[tokio::test]
    async fn test_maybe_compact_triggers_and_shrinks_history_with_tiny_policy() -> TestResult {
        // Winziges Kontextfenster ⇒ winzige Schwellen (70/30 Tokens), leicht
        // von einer kleinen künstlichen Nutzung überschritten.
        let policy = crate::auto_compact::AutoCompactPolicy::for_context_window(100);
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
            .with_auto_compact(Some(policy));
        let observer = RecordingCompactionObserver::new();
        session = session.with_compaction_observer(Some(observer.clone()));
        // Genug wiederholte, deterministisch deduplizierbare Items, damit
        // `deterministic_pass` die Historie sichtbar schrumpft.
        for i in 0..20 {
            session.history_mut().push_user_text("frage");
            session.history_mut().push_assistant_text(
                format!("wiederholte, ausführliche Antwort Nummer {i} mit etwas mehr Text"),
                None,
            );
        }
        let bytes_before: usize = session
            .history()
            .items()
            .iter()
            .map(|item| serde_json::to_vec(item).map(|v| v.len()).unwrap_or(0))
            .sum();
        let store = crate::state_store::InMemoryStateStore::new();
        // Über der Task-Ende-Schwelle (30), unter der Budget-Schwelle (70)
        // ⇒ `CompactDecision::TaskCompleted`.
        let usage = harw_types::TokenUsage {
            cache_separate: false,
            input_tokens: 40,
            output_tokens: 0,
            reasoning_tokens: None,
            cached_tokens: None,
            cache_write_tokens: None,
        };

        maybe_compact(
            &mut session,
            &crate::model::EchoModelProvider::default(),
            &store,
            &usage,
            true,
        )
        .await;

        let outcome = observer
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or(TestError::Missing(
                "policy set and thresholds exceeded ⇒ compact_session must run",
            ))?;
        assert_eq!(
            outcome.reason,
            Some(crate::auto_compact::CompactDecision::TaskCompleted)
        );
        let bytes_after: usize = session
            .history()
            .items()
            .iter()
            .map(|item| serde_json::to_vec(item).map(|v| v.len()).unwrap_or(0))
            .sum();
        assert!(
            bytes_after <= bytes_before,
            "compaction must not grow the history (before={bytes_before}, after={bytes_after})"
        );
        Ok(())
    }

    // ------------------------------------------------------------------
    // maybe_hard_compact_at_turn_start tests (Addendum D)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_turn_start_hard_compact_noop_without_target() {
        // Policy ohne `with_turn_start_target` ⇒ kein Turn-Start-Compact,
        // unabhängig von der Verlaufsgröße (Worker-Kinder betrifft das).
        let policy = crate::auto_compact::AutoCompactPolicy::for_context_window(200_000);
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
            .with_auto_compact(Some(policy));
        let observer = RecordingCompactionObserver::new();
        session = session.with_compaction_observer(Some(observer.clone()));
        for i in 0..20 {
            session.history_mut().push_user_text("frage");
            session
                .history_mut()
                .push_assistant_text(format!("antwort {i}"), None);
        }
        let store = crate::state_store::InMemoryStateStore::new();

        maybe_hard_compact_at_turn_start(
            &mut session,
            &crate::model::EchoModelProvider::default(),
            &store,
        )
        .await;

        assert!(
            observer
                .last
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_none(),
            "no turn_start_target ⇒ compact_session must never run"
        );
    }

    #[tokio::test]
    async fn test_turn_start_hard_compact_triggers_above_target() -> TestResult {
        // Winziges Ziel ⇒ die künstlich aufgeblähte Historie liegt sicher
        // darüber und löst die harte Verdichtung aus.
        let policy = crate::auto_compact::AutoCompactPolicy::for_context_window(200_000)
            .with_turn_start_target(Some(10));
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
            .with_auto_compact(Some(policy));
        let observer = RecordingCompactionObserver::new();
        session = session.with_compaction_observer(Some(observer.clone()));
        for i in 0..20 {
            session.history_mut().push_user_text("frage");
            session.history_mut().push_assistant_text(
                format!("wiederholte, ausführliche Antwort Nummer {i} mit etwas mehr Text"),
                None,
            );
        }
        let store = crate::state_store::InMemoryStateStore::new();

        maybe_hard_compact_at_turn_start(
            &mut session,
            &crate::model::EchoModelProvider::default(),
            &store,
        )
        .await;

        let outcome = observer
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or(TestError::Missing(
                "history far above the tiny target ⇒ compact_session must run",
            ))?;
        assert_eq!(
            outcome.reason,
            Some(crate::auto_compact::CompactDecision::TurnStart)
        );
        Ok(())
    }

    // ------------------------------------------------------------------
    // last_round_usage → maybe_compact wiring (Teil 1)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn test_last_round_usage_from_response_drives_auto_compaction() -> TestResult {
        // `last_round_usage` wird in `drive_turn` bei jeder Modellrunde aus
        // `response.usage` gesetzt und an `maybe_compact` weitergereicht
        // (Turn-Ende-Aufruf). Mit `TokenUsage::default()` (der ungenutzte
        // Anfangswert) könnte `decide()` bei einer 70-Token-Schwelle nie
        // `BudgetExceeded` liefern — nur die tatsächliche Antwort-Nutzung
        // (80 Tokens) kann das. Der Test beweist also konkret, dass der
        // reale Wert ankommt, nicht der Default.
        let policy = crate::auto_compact::AutoCompactPolicy::for_context_window(100);
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full))
            .with_auto_compact(Some(policy));
        let observer = RecordingCompactionObserver::new();
        session = session.with_compaction_observer(Some(observer.clone()));
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![crate::model::ModelResponse {
            usage: harw_types::TokenUsage {
                cache_separate: false,
                input_tokens: 80,
                output_tokens: 5,
                reasoning_tokens: None,
                cached_tokens: None,
                cache_write_tokens: None,
            },
            ..crate::model::ModelResponse::text("fertig")
        }]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("hi"))
            .await
            .map_err(ctx("der Turn läuft durch"))?;
        assert!(matches!(outcome, TurnOutcome::Completed));

        let compaction_outcome = observer
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or(TestError::Missing(
                "die tatsächliche Antwort-Nutzung (80 > 70er-Schwelle) muss den \
                 Turn-Ende-Compact auslösen — geschieht das nicht, wurde \
                 `last_round_usage` nicht an `maybe_compact` durchgereicht",
            ))?;
        assert_eq!(
            compaction_outcome.reason,
            Some(crate::auto_compact::CompactDecision::BudgetExceeded)
        );
        Ok(())
    }

    // ------------------------------------------------------------------
    // Reasoning-Sichtbarkeit für die UIA-Root-Session (Teil 2 / Welle 3 — 3e)
    // ------------------------------------------------------------------

    /// UIA-Root-Fixture: kein Parent, `organizational_role` =
    /// `AgentRoleId::UserInterface` — identisches Kriterium zu
    /// `is_uia_root_session` in `harw-tui/src/session_controller.rs`.
    fn uia_root_session(provider: Arc<dyn ToolProvider>) -> TestResult<AgentSession> {
        let (tx, _rx) = mpsc::unbounded_channel();
        let registry = ExtensionRegistryBuilder::default()
            .tool_provider(provider)
            .build();
        Ok(
            AgentSession::new(AgentRole::Assistant, None, registry, tx).with_spawn_context(
                SpawnContext {
                    organizational_role: harw_agent_dsl::roles::AgentRoleId::UserInterface,
                    ..test_spawn_context()?
                },
            ),
        )
    }

    fn anthropic_reasoning(blocks: Vec<serde_json::Value>) -> OpaqueReasoning {
        OpaqueReasoning {
            provider: "anthropic".to_owned(),
            model: "claude-test".to_owned(),
            blocks,
        }
    }

    #[tokio::test]
    async fn test_thinking_block_emits_reasoning_item_for_uia_root_session() -> TestResult {
        let provider = StubToolProvider::with_names(&[]);
        let mut session = uia_root_session(provider)?;
        let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
        session = session.with_turn_event_sink(turn_tx);
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![crate::model::ModelResponse {
            reasoning: Some(anthropic_reasoning(vec![serde_json::json!({
                "type": "thinking",
                "thinking": "Der Nutzer fragt nach X, also prüfe ich zuerst Y.",
            })])),
            ..crate::model::ModelResponse::text("Antwort")
        }]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("frage"))
            .await
            .map_err(ctx("der Turn läuft durch"))?;
        assert!(matches!(outcome, TurnOutcome::Completed));

        // History: Reasoning-Item kommt vor der AssistantMessage.
        let items = session.history().items();
        let reasoning_pos = items
            .iter()
            .position(|item| matches!(item, TurnItem::Reasoning(_)))
            .ok_or(TestError::Missing(
                "ein Reasoning-Item muss in der History stehen",
            ))?;
        let assistant_pos = items
            .iter()
            .position(|item| matches!(item, TurnItem::AssistantMessage(_)))
            .ok_or(TestError::Missing(
                "die AssistantMessage muss in der History stehen",
            ))?;
        assert!(
            reasoning_pos < assistant_pos,
            "Reasoning muss vor der AssistantMessage stehen (Denken vor Antwort)"
        );
        match &items[reasoning_pos] {
            TurnItem::Reasoning(item) => {
                assert_eq!(
                    item.summary_text,
                    vec!["Der Nutzer fragt nach X, also prüfe ich zuerst Y.".to_owned()]
                );
                assert!(item.raw_content.is_empty());
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartete TurnItem::Reasoning, war {other:?}"
                )));
            }
        }

        // Event: TurnEvent::ItemAdded mit TurnItem::Reasoning wurde emittiert.
        let events = drain_turn_events(&mut turn_rx);
        let reasoning_event = events.iter().find(|event| {
            matches!(
                event,
                TurnEvent::ItemAdded {
                    item: TurnItem::Reasoning(_),
                    ..
                }
            )
        });
        assert!(
            reasoning_event.is_some(),
            "ein TurnEvent::ItemAdded für das Reasoning-Item muss emittiert werden"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_thinking_block_emits_no_reasoning_item_for_non_uia_session() -> TestResult {
        // Identische Antwort wie im UIA-Test, aber eine Session mit
        // Standard-Rolle (`test_spawn_context()` ⇒ `RootOrchestrator`) —
        // das UIA-Filter-Kriterium darf hier nicht greifen.
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![crate::model::ModelResponse {
            reasoning: Some(anthropic_reasoning(vec![serde_json::json!({
                "type": "thinking",
                "thinking": "unsichtbarer Denkprozess",
            })])),
            ..crate::model::ModelResponse::text("Antwort")
        }]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("frage"))
            .await
            .map_err(ctx("der Turn läuft durch"))?;
        assert!(matches!(outcome, TurnOutcome::Completed));

        assert!(
            !session
                .history()
                .items()
                .iter()
                .any(|item| matches!(item, TurnItem::Reasoning(_))),
            "eine Nicht-UIA-Session darf kein Reasoning-Item bekommen"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_redacted_thinking_emits_no_reasoning_item_and_does_not_error() -> TestResult {
        // `redacted_thinking`-Blöcke tragen keinen lesbaren Text — die
        // Extraktion muss robust `None` liefern statt zu paniken, auch für
        // eine UIA-Root-Session.
        let provider = StubToolProvider::with_names(&[]);
        let mut session = uia_root_session(provider)?;
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![crate::model::ModelResponse {
            reasoning: Some(anthropic_reasoning(vec![serde_json::json!({
                "type": "redacted_thinking",
                "data": "verschlüsselter-blob",
            })])),
            ..crate::model::ModelResponse::text("Antwort")
        }]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("frage"))
            .await
            .map_err(ctx(
                "ein redacted_thinking-Block darf den Turn nicht scheitern lassen",
            ))?;
        assert!(matches!(outcome, TurnOutcome::Completed));

        assert!(
            !session
                .history()
                .items()
                .iter()
                .any(|item| matches!(item, TurnItem::Reasoning(_))),
            "redacted_thinking liefert keinen extrahierbaren Text ⇒ kein Reasoning-Item"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_no_reasoning_data_leaves_behavior_unchanged() -> TestResult {
        // `response.reasoning == None` (kein Extended Thinking) — auch für
        // eine UIA-Root-Session darf sich am bisherigen Verhalten nichts
        // ändern: nur die AssistantMessage landet in der History.
        let provider = StubToolProvider::with_names(&[]);
        let mut session = uia_root_session(provider)?;
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![crate::model::ModelResponse::text(
            "Antwort ohne Denken",
        )]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("frage"))
            .await
            .map_err(ctx("der Turn läuft durch"))?;
        assert!(matches!(outcome, TurnOutcome::Completed));

        let items = session.history().items();
        assert!(
            !items
                .iter()
                .any(|item| matches!(item, TurnItem::Reasoning(_))),
            "ohne Reasoning-Daten darf kein Reasoning-Item entstehen"
        );
        assert!(
            items
                .iter()
                .any(|item| matches!(item, TurnItem::AssistantMessage(_))),
            "die AssistantMessage muss weiterhin in der History stehen"
        );
        Ok(())
    }

    #[test]
    fn test_extract_thinking_text_joins_multiple_thinking_blocks() {
        let reasoning = anthropic_reasoning(vec![
            serde_json::json!({"type": "thinking", "thinking": "erster Gedanke"}),
            serde_json::json!({"type": "thinking", "thinking": "zweiter Gedanke"}),
        ]);
        assert_eq!(
            extract_thinking_text(&reasoning),
            Some("erster Gedanke\nzweiter Gedanke".to_owned())
        );
    }

    #[test]
    fn test_extract_thinking_text_skips_redacted_blocks() {
        let reasoning = anthropic_reasoning(vec![serde_json::json!({
            "type": "redacted_thinking",
            "data": "opak",
        })]);
        assert_eq!(extract_thinking_text(&reasoning), None);
    }

    #[test]
    fn test_extract_thinking_text_handles_missing_field_without_panicking() {
        // Ein `thinking`-Block ohne das `thinking`-Textfeld (z. B. ein
        // zukünftiges Provider-Format) darf nicht paniken — nur ausgelassen
        // werden.
        let reasoning = anthropic_reasoning(vec![serde_json::json!({"type": "thinking"})]);
        assert_eq!(extract_thinking_text(&reasoning), None);
    }

    #[test]
    fn test_extract_thinking_text_none_for_empty_blocks() {
        let reasoning = anthropic_reasoning(vec![]);
        assert_eq!(extract_thinking_text(&reasoning), None);
    }

    #[test]
    fn test_extract_thinking_text_joins_reasoning_summary_text_entries() {
        // OpenAI-Responses-Transport: ein "reasoning"-Block trägt mehrere
        // summary_text-Einträge, die verkettet werden müssen.
        let reasoning = anthropic_reasoning(vec![serde_json::json!({
            "type": "reasoning",
            "summary": [
                {"type": "summary_text", "text": "erster Abschnitt"},
                {"type": "summary_text", "text": "zweiter Abschnitt"},
            ],
        })]);
        assert_eq!(
            extract_thinking_text(&reasoning),
            Some("erster Abschnitt\nzweiter Abschnitt".to_owned())
        );
    }

    #[test]
    fn test_extract_thinking_text_takes_reasoning_content_text() {
        // Chat-Transport (DeepSeek/Kimi/GLM-Konvention): Feld "text" wird
        // direkt übernommen.
        let reasoning = anthropic_reasoning(vec![serde_json::json!({
            "type": "reasoning_content",
            "text": "Gedankengang aus reasoning_content",
        })]);
        assert_eq!(
            extract_thinking_text(&reasoning),
            Some("Gedankengang aus reasoning_content".to_owned())
        );
    }

    #[test]
    fn test_extract_thinking_text_skips_unknown_block_type() {
        // Ein Block, der weder "thinking" noch "reasoning" noch
        // "reasoning_content" ist, darf weiterhin kein Item/keinen Fehler
        // erzeugen — nur stillschweigend übersprungen werden.
        let reasoning = anthropic_reasoning(vec![serde_json::json!({
            "type": "some_future_block_type",
            "text": "sollte ignoriert werden",
        })]);
        assert_eq!(extract_thinking_text(&reasoning), None);
    }

    #[test]
    fn test_extract_thinking_text_none_for_empty_reasoning_summary_array() {
        // Ein leeres summary-Array darf nicht als „gefunden" zählen — kein
        // leerer String, sondern `None`.
        let reasoning = anthropic_reasoning(vec![serde_json::json!({
            "type": "reasoning",
            "summary": [],
        })]);
        assert_eq!(extract_thinking_text(&reasoning), None);
    }
}

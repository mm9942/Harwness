//! `harw-matrix-game` — deterministischer Kern des Matrix Games
//! (`docs/design/matrix-game.md`, Business-Modus nach
//! `docs/design/wargaming-and-analysis.md` §1).
//!
//! Der GameMaster besitzt Spielzustand, Journal, RNG und die Entscheidung,
//! *wer was sieht*. Es gibt kein Agent-zu-Agent-Messaging: Jede Nachricht
//! ist ein Journal-Eintrag mit einer [`state::Audience`], und jeder Prompt
//! entsteht ausschließlich aus der Projektion [`visibility::SeatView`].
//! LLM-Ausgaben sind die einzige nicht-deterministische Quelle; sie werden
//! journalisiert, sodass [`state::replay`] ohne Modellaufrufe denselben
//! Zustand (und dieselben `state_hash`es und Würfel) ergibt.
//!
//! Dieses Crate enthält **keine** Modellaufrufe. Module:
//! - [`scenario`]: `harwness.matrix-scenario/v1` (klassisch und `business`),
//!   Parsing und Validierung.
//! - [`state`]: Sitze, Audiences, Weltvariablen, Journal (JSONL), Zustand,
//!   Replay.
//! - [`visibility`]: `project(journal, seat)`, Leak-Guards (5-Gramm-Scanner).
//! - [`commitments`]: SHA-256-Commitments für versiegelte/geheime Argumente.
//! - [`dice`]: abgeleitete ChaCha20-Seeds, 2W6-Pro/Contra-Mathematik, W100-Leiter.
//! - [`phases`]: Phasen-FSM, JSON-Contracts, Effekt-Validierung,
//!   deterministische GameMaster-Schritte.
//! - [`aar`]: After-Action-Review als Markdown.
//! - [`events`]: [`events::MatrixGameEvent`] für das TUI-Panel.
//!
//! # Concurrency
//! Alle Typen sind reine Daten ohne interne Mutabilität. Parallele
//! Child-Aufrufe sammelt der Aufrufer; eingefügt wird in kanonischer
//! Sitzreihenfolge ([`phases::ArgumentBox`], [`phases::reveal_round`]).
//!
//! # Offene Punkte (bewusst nicht im ersten Schnitt)
//! Business-Marktmodell (`logit_share`) und Auswertung von `when`-Prädikaten,
//! Knock-out-Würfe der Schlussargumente, `argument_mode = "sequential"`,
//! `deferred`-Effekte geheimer Argumente, Konflikt-Integration in
//! [`phases::resolve_argument`] (Mathematik in [`dice::resolve_conflict`]).

pub mod aar;
pub mod commitments;
pub mod dice;
pub mod error;
pub mod events;
pub mod phases;
pub mod scenario;
pub mod state;
pub mod visibility;

pub use error::{MatrixError, MatrixResult};

/// Gemeinsame Test-Fixtures: Beispielszenarien und ein geskriptetes Spiel.
#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::BTreeMap;

    use crate::phases::{
        ArgumentBox, BriefingAck, CounterArgument, CounterEntry, EffectOp, Narration,
        NegotiationMessage, NegotiationMessages, NegotiationOpening, NegotiationRequest,
        PhaseConfig, PlayerArgument, Proposal, RoundCursor, UmpireAdjudication, UmpireCon,
        UmpireNarration, UmpireRuling, Verdict, apply_inject, close_negotiation, close_round,
        end_game, enter_phase, open_channels, open_game, post_messages, record_briefing,
        record_narration, record_standing, resolve_argument, reveal_round, submit_counters,
        validate_negotiation_messages, validate_negotiation_request, validate_player_argument,
        validate_umpire_adjudication,
    };
    use crate::scenario::{LoadedScenario, load_scenario};
    use crate::state::{Audience, AudienceSpec, GameLog, PlayerId, Seat};

    pub(crate) type Fixture<T> = Result<T, Box<dyn std::error::Error>>;

    /// Klassisches Beispielszenario (matrix-game.md §5.1).
    pub(crate) const KARST: &str = include_str!("../scenarios/karst-islands.toml");
    /// Business-Beispielszenario (wargaming-and-analysis.md §1.9).
    pub(crate) const CLOUD: &str = include_str!("../scenarios/cloud-sme-2027.toml");

    /// Eröffnung des Rats im Paar-Kanal mit dem Nordreich.
    pub(crate) const RAT_OPENING: &str =
        "Wir brauchen Tankschiffe aber ohne Fahnen am Heck und ohne Presse.";
    /// Antwort des Nordreichs.
    pub(crate) const NORD_REPLY: &str =
        "Gegen ein Landerecht am Nordkai liefern wir zwei Tankschiffe noch diese Woche.";

    fn argument(action: &str, pros: &[&str], secret: bool, note: Option<&str>) -> PlayerArgument {
        PlayerArgument {
            action: action.to_owned(),
            pros: pros.iter().map(|p| (*p).to_owned()).collect(),
            secret,
            cites_negotiation: Vec::new(),
            conflict_target: None,
            project: None,
            use_fail_chit_if_failed: false,
            private_note: note.map(str::to_owned),
        }
    }

    /// Einreichungen der ersten Runde (die Mission passt).
    pub(crate) fn scripted_submissions() -> BTreeMap<PlayerId, PlayerArgument> {
        let mut subs = BTreeMap::new();
        subs.insert(
            PlayerId::new("rat"),
            argument(
                "Der Inselrat stellt die Entsalzungsanlage unter Polizeischutz.",
                &[
                    "Die Inselpolizei ist bereits vor Ort.",
                    "Der Rat hat das Mandat der Wähler.",
                ],
                false,
                None,
            ),
        );
        subs.insert(
            PlayerId::new("gilde"),
            argument(
                "Die Gilde schleust nachts zusätzliche Tanker am Zoll vorbei.",
                &[
                    "Die Hafengilde kontrolliert die Kräne.",
                    "Der Zoll ist unterbesetzt.",
                ],
                true,
                Some("Nur für uns: das Netz muss bis Runde drei stehen."),
            ),
        );
        subs.insert(
            PlayerId::new("nord"),
            argument(
                "Das Nordreich liefert zwei Tankschiffe Trinkwasser nach Velmar.",
                &[
                    "Das Nordreich hat Überschusswasser.",
                    "Die Botschaft hat die Lieferung angekündigt.",
                    "Die Tankschiffe liegen bereits im Nordhafen.",
                ],
                false,
                None,
            ),
        );
        subs
    }

    fn ruling(argument_id: &str) -> UmpireRuling {
        UmpireRuling {
            argument_id: argument_id.to_owned(),
            verdict: Verdict::Roll,
            pro_weights: Vec::new(),
            con_weights: BTreeMap::new(),
            umpire_cons: Vec::new(),
            context_modifier: 0,
            context_reason: None,
            probability: None,
            inconsistent_with: None,
            public_rationale: None,
            private_notes: None,
            on_success: Vec::new(),
            on_failure: Vec::new(),
            triggers_secret: None,
        }
    }

    fn scripted_adjudication() -> UmpireAdjudication {
        let mut a1 = ruling("r1-a1");
        a1.pro_weights = vec![2, 1];
        a1.con_weights.insert("nord".to_owned(), vec![1]);
        a1.public_rationale = Some("Die Polizei ist vor Ort, das Mandat trägt.".to_owned());
        a1.on_success = vec![EffectOp::Add {
            var: "stability".to_owned(),
            by: 1,
        }];
        a1.on_failure = vec![EffectOp::Add {
            var: "rat_support".to_owned(),
            by: -1,
        }];

        let mut a2 = ruling("r1-a2");
        a2.pro_weights = vec![1, 1];
        a2.umpire_cons = vec![UmpireCon {
            text: "Der Zoll wurde verstärkt.".to_owned(),
            weight: 1,
        }];
        a2.private_notes = Some("Riskant, aber plausibel.".to_owned());
        a2.on_success = vec![EffectOp::Add {
            var: "smuggling_net".to_owned(),
            by: 1,
        }];
        a2.on_failure = vec![EffectOp::Fact {
            text: "Ein Tanker wurde vom Zoll aufgehalten.".to_owned(),
            audience: AudienceSpec(Audience::Seat(PlayerId::new("gilde"))),
        }];

        let mut a3 = ruling("r1-a3");
        a3.pro_weights = vec![2, 1, 0];
        a3.con_weights.insert("rat".to_owned(), vec![1, 1]);
        a3.context_modifier = 1;
        a3.context_reason = Some("Nachbarschaftshilfe hat Präzedenz.".to_owned());
        a3.public_rationale = Some("Logistisch machbar, politisch heikel.".to_owned());
        a3.on_success = vec![
            EffectOp::Add {
                var: "water".to_owned(),
                by: 1,
            },
            EffectOp::Add {
                var: "north_influence".to_owned(),
                by: 1,
            },
        ];
        a3.on_failure = vec![EffectOp::Fact {
            text: "Die Tankschiffe bleiben im Sturm liegen.".to_owned(),
            audience: AudienceSpec(Audience::Public),
        }];

        UmpireAdjudication {
            rulings: vec![a1, a2, a3],
            conflicts: Vec::new(),
            standing: vec![
                "nord".to_owned(),
                "rat".to_owned(),
                "gilde".to_owned(),
                "mission".to_owned(),
            ],
        }
    }

    /// Spielt Runde 1 vollständig und beendet das Spiel in Runde 2.
    pub(crate) fn scripted_game(
        master: &[u8; 32],
        completion_order: &[&str],
    ) -> Fixture<(LoadedScenario, GameLog)> {
        scripted_game_with(master, completion_order, true)
    }

    /// Wie [`scripted_game`], optional ohne die Verhandlung Rat ⇄ Nordreich.
    /// `completion_order` simuliert die Fertigstellungsreihenfolge paralleler
    /// Child-Aufrufe in der Argumente-Phase.
    pub(crate) fn scripted_game_with(
        master: &[u8; 32],
        completion_order: &[&str],
        negotiate: bool,
    ) -> Fixture<(LoadedScenario, GameLog)> {
        let loaded = load_scenario(KARST)?;
        let scenario = loaded.scenario.clone();
        let rules = scenario.rules().clone();
        let cfg = PhaseConfig::from_scenario(&scenario);
        let mut log = open_game(&loaded, master, None)?;
        let seats = scenario.seat_order();

        // Briefing
        let mut cursor = RoundCursor::start().next(&cfg).ok_or("FSM")?;
        enter_phase(&mut log, cursor, None)?;
        for inject in scenario.injects_for_round(cursor.round) {
            apply_inject(&mut log, &rules, &inject, None)?;
        }
        for p in &seats {
            let ack = BriefingAck {
                ack: true,
                intent: Some(format!("Absicht von {p}")),
            };
            record_briefing(&mut log, &Seat::Player(p.clone()), &ack, None)?;
        }

        // Verhandlung
        cursor = cursor.next(&cfg).ok_or("FSM")?;
        enter_phase(&mut log, cursor, None)?;
        if negotiate {
            let rat = PlayerId::new("rat");
            let nord = PlayerId::new("nord");
            let request = NegotiationRequest {
                requests: vec![NegotiationOpening {
                    to: "nord".to_owned(),
                    opening: RAT_OPENING.to_owned(),
                }],
            };
            validate_negotiation_request(&request, &rat, &seats, &rules)?;
            let mut requests = BTreeMap::new();
            requests.insert(rat, request);
            let opened = open_channels(&mut log, &requests, None)?;
            let channel = opened.first().cloned().ok_or("kein Kanal")?;
            let reply = NegotiationMessages {
                messages: vec![NegotiationMessage {
                    channel,
                    text: NORD_REPLY.to_owned(),
                    proposal: Some(Proposal {
                        summary: "Tankschiffe gegen Landerecht".to_owned(),
                    }),
                    accept: None,
                    decline: false,
                }],
            };
            validate_negotiation_messages(&reply, &nord, &log.state, &rules)?;
            post_messages(&mut log, &nord, &reply, None)?;
        }
        close_negotiation(&mut log, None)?;

        // Argumente (versiegelt, Fertigstellungsreihenfolge variabel)
        cursor = cursor.next(&cfg).ok_or("FSM")?;
        enter_phase(&mut log, cursor, None)?;
        let subs = scripted_submissions();
        let mut sealed = ArgumentBox::new(cursor.round);
        for p in completion_order {
            let player = PlayerId::new(*p);
            match subs.get(&player) {
                Some(arg) => {
                    validate_player_argument(arg, &player, &log.state, &rules, false)?;
                    sealed.seal(player, arg.clone())?;
                }
                None => sealed.forfeit(player)?,
            }
        }
        let mut args = reveal_round(&mut log, &scenario, sealed, None)?;

        // Gegenargumente (kanonische Reihenfolge)
        cursor = cursor.next(&cfg).ok_or("FSM")?;
        enter_phase(&mut log, cursor, None)?;
        let mut counters: BTreeMap<PlayerId, CounterArgument> = BTreeMap::new();
        counters.insert(
            PlayerId::new("rat"),
            CounterArgument {
                counters: vec![CounterEntry {
                    argument_id: "r1-a3".to_owned(),
                    cons: vec![
                        "Das Nordreich will nur Einfluss gewinnen.".to_owned(),
                        "Der Hafen ist für große Tanker zu flach.".to_owned(),
                    ],
                }],
            },
        );
        counters.insert(
            PlayerId::new("nord"),
            CounterArgument {
                counters: vec![CounterEntry {
                    argument_id: "r1-a1".to_owned(),
                    cons: vec!["Die Polizei ist erschöpft.".to_owned()],
                }],
            },
        );
        for p in &seats {
            let counter = counters.get(p).cloned().unwrap_or_default();
            submit_counters(&mut log, &mut args, p, &counter, None)?;
        }

        // Adjudikation
        cursor = cursor.next(&cfg).ok_or("FSM")?;
        enter_phase(&mut log, cursor, None)?;
        let adjudication = scripted_adjudication();
        validate_umpire_adjudication(&adjudication, &args, &log.state, &rules)?;
        record_standing(&mut log, &adjudication.standing, None)?;
        for arg in &args {
            let ruling = adjudication
                .rulings
                .iter()
                .find(|r| r.argument_id == arg.id)
                .ok_or("Urteil fehlt")?;
            resolve_argument(&mut log, &scenario, arg, ruling, None)?;
        }
        let narration = UmpireNarration {
            narrations: vec![Narration {
                argument_id: "r1-a1".to_owned(),
                audience: AudienceSpec(Audience::Public),
                text: "Polizisten ziehen vor der Anlage auf.".to_owned(),
            }],
            round_summary: Some("Eine angespannte erste Runde.".to_owned()),
        };
        record_narration(&mut log, &args, &narration, None)?;

        // Veröffentlichung, Rundenende
        cursor = cursor.next(&cfg).ok_or("FSM")?;
        enter_phase(&mut log, cursor, None)?;
        cursor = cursor.next(&cfg).ok_or("FSM")?;
        enter_phase(&mut log, cursor, None)?;
        close_round(&mut log, &rules, None)?;

        // Runde 2: Briefing, dann Facilitator-Ende
        cursor = cursor.next(&cfg).ok_or("FSM")?;
        enter_phase(&mut log, cursor, None)?;
        end_game(&mut log, "Facilitator: Ende", None)?;
        Ok((loaded, log))
    }

    #[test]
    fn scripted_game_runs_for_both_modes() -> Fixture<()> {
        let (_, log) = scripted_game(&[1u8; 32], &["rat", "gilde", "nord", "mission"])?;
        assert!(log.state.ended);
        assert_eq!(log.state.outcomes.len(), 4);
        let business = load_scenario(CLOUD)?;
        let blog = open_game(&business, &[2u8; 32], None)?;
        assert_eq!(blog.state.players.len(), 4);
        assert!(blog.state.vars.contains_key("competitor_a.share.sme"));
        Ok(())
    }
}

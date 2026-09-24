//! Querschnittstests der Erweiterungen (matrix-game.md §11): Verhaltensprofil,
//! Red Cell, Verdachtsleiter, Präzedenzregister, Inject-Bibliothek und
//! Design-Lehren — jeweils mit Sichtbarkeits- und Replay-Invarianten.

use std::collections::BTreeMap;

use crate::aar::{AarInput, build_aar};
use crate::error::MatrixError;
use crate::library::{package_injects_for_round, record_inject_package};
use crate::phases::{
    ArgumentBox, BriefingAck, ContractKind, CounterArgument, CounterEntry, EffectOp, ExpectedCall,
    PhaseConfig, RedCellObjection, RoundArgument, RoundCursor, UmpireAdjudication, UmpireCon,
    close_negotiation, close_round, end_game, enter_phase, open_game, record_briefing,
    record_standing, resolve_argument, reveal_round, submit_counters, submit_red_cell,
    validate_red_cell_objection, validate_umpire_adjudication,
};
use crate::precedents::PrecedentFlag;
use crate::prompts::{SeatRole, system_prompt, turn_prompt};
use crate::scenario::{LoadedScenario, RED_CELL_KEY, load_scenario};
use crate::state::{EntryKind, GameLog, PlayerId, RevealedBy, Seat, SuspicionLevel, replay};
use crate::test_support::{
    Fixture, KARST, ORDER, RAT_RED_LINE, argument, karst_extended, ruling, scripted_submissions,
};
use crate::visibility::{SeatView, VisibilityCfg, project};

/// Inhalt des geheimen Gilde-Arguments (`s1`) aus den Skript-Einreichungen.
const SECRET_ACTION: &str = "Die Gilde schleust nachts zusätzliche Tanker am Zoll vorbei.";
/// Maßstab des Präzedenzfalls aus Runde 1.
const PRINCIPLE: &str =
    "Wer Schutz übernimmt, braucht Leute vor Ort; ein Mandat allein trägt nicht.";
/// Kernannahme, die die Red Cell in Runde 1 angreift.
const ASSUMPTION: &str = "Die Polizei ist stark genug, die Anlage dauerhaft zu schützen.";

fn view(loaded: &LoadedScenario, log: &GameLog, seat: &Seat) -> SeatView {
    project(
        &log.journal,
        seat,
        &VisibilityCfg::from_settings(loaded.scenario.visibility()),
    )
}

fn prompt(loaded: &LoadedScenario, log: &GameLog, seat: &Seat, contract: ContractKind) -> String {
    let v = view(loaded, log, seat);
    turn_prompt(
        &v,
        0,
        &ExpectedCall {
            seat: seat.clone(),
            contract,
        },
        true,
    )
}

fn begin_round(
    log: &mut GameLog,
    loaded: &LoadedScenario,
    cursor: RoundCursor,
    cfg: &PhaseConfig,
) -> Fixture<RoundCursor> {
    let scenario = &loaded.scenario;
    let rules = scenario.rules();
    let briefing = cursor.next(cfg).ok_or("FSM")?;
    enter_phase(log, briefing, None)?;
    let mut injects = scenario.injects_for_round(briefing.round);
    injects.extend(package_injects_for_round(
        scenario,
        &log.journal,
        briefing.round,
    ));
    for inject in injects {
        crate::phases::apply_inject(log, rules, &inject, None)?;
    }
    for p in scenario.seat_order() {
        record_briefing(
            log,
            &Seat::Player(p),
            &BriefingAck {
                ack: true,
                intent: None,
            },
            None,
        )?;
    }
    let negotiation = briefing.next(cfg).ok_or("FSM")?;
    enter_phase(log, negotiation, None)?;
    close_negotiation(log, None)?;
    let arguments = negotiation.next(cfg).ok_or("FSM")?;
    enter_phase(log, arguments, None)?;
    Ok(arguments)
}

fn finish_round(
    log: &mut GameLog,
    loaded: &LoadedScenario,
    cursor: RoundCursor,
    cfg: &PhaseConfig,
) -> Fixture<RoundCursor> {
    let publish = cursor.next(cfg).ok_or("FSM")?;
    enter_phase(log, publish, None)?;
    let end = publish.next(cfg).ok_or("FSM")?;
    enter_phase(log, end, None)?;
    close_round(log, loaded.scenario.rules(), None)?;
    Ok(end)
}

/// Runde-1-Urteil: Rat mit Präzedenzfall und Verdacht auf `s1`, Gilde geheim,
/// Nordreich mit Contras des Rats.
fn round_one_adjudication() -> UmpireAdjudication {
    let raise = vec![EffectOp::RaiseSuspicion {
        secret_id: "s1".to_owned(),
        by: 2,
    }];
    let mut a1 = ruling("r1-a1");
    a1.pro_weights = vec![2, 1];
    a1.con_weights.insert("nord".to_owned(), vec![1]);
    a1.con_weights.insert(RED_CELL_KEY.to_owned(), vec![1, 1]);
    a1.public_rationale = Some("Die Polizei ist vor Ort.".to_owned());
    a1.on_success.clone_from(&raise);
    a1.on_failure = raise;
    a1.precedent = Some(PrecedentFlag {
        principle: PRINCIPLE.to_owned(),
        tags: vec!["Polizei".to_owned(), "Schutz".to_owned()],
    });

    let mut a2 = ruling("r1-a2");
    a2.pro_weights = vec![1, 1];
    a2.umpire_cons = vec![UmpireCon {
        text: "Der Zoll wurde verstärkt.".to_owned(),
        weight: 1,
    }];

    let mut a3 = ruling("r1-a3");
    a3.pro_weights = vec![2, 1, 0];
    a3.con_weights.insert("rat".to_owned(), vec![1, 1]);

    UmpireAdjudication {
        rulings: vec![a1, a2, a3],
        conflicts: Vec::new(),
        standing: vec!["nord".to_owned(), "rat".to_owned()],
    }
}

struct RoundOne {
    loaded: LoadedScenario,
    log: GameLog,
    cursor: RoundCursor,
    cfg: PhaseConfig,
    args: Vec<RoundArgument>,
}

/// Spielt Runde 1 des erweiterten Karst-Szenarios bis zum Rundenende.
fn round_one(master: &[u8; 32]) -> Fixture<RoundOne> {
    let loaded = load_scenario(&karst_extended()?)?;
    let scenario = loaded.scenario.clone();
    let cfg = PhaseConfig::from_scenario(&scenario);
    let mut log = open_game(&loaded, master, None)?;
    record_inject_package(&mut log, &scenario, Some("unruhen"), None)?;
    let mut cursor = begin_round(&mut log, &loaded, RoundCursor::start(), &cfg)?;

    let subs = scripted_submissions();
    let mut sealed = ArgumentBox::new(cursor.round);
    for p in ORDER {
        let player = PlayerId::new(p);
        match subs.get(&player) {
            Some(arg) => sealed.seal(player, arg.clone())?,
            None => sealed.forfeit(player)?,
        }
    }
    let mut args = reveal_round(&mut log, &scenario, sealed, None)?;

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
    for p in scenario.seat_order() {
        let counter = counters.get(&p).cloned().unwrap_or_default();
        submit_counters(&mut log, &mut args, &p, &counter, None)?;
    }
    let objection = RedCellObjection {
        no_objection: false,
        target: Some("r1-a1".to_owned()),
        assumption: Some(ASSUMPTION.to_owned()),
        cons: vec![
            "Die Polizei arbeitet seit Wochen im Schichtbetrieb am Limit.".to_owned(),
            "Ein Teil der Beamten wohnt im Hafenviertel und ist befangen.".to_owned(),
        ],
    };
    submit_red_cell(&mut log, &scenario, &mut args, &objection, None)?;

    cursor = cursor.next(&cfg).ok_or("FSM")?;
    enter_phase(&mut log, cursor, None)?;
    let adjudication = round_one_adjudication();
    validate_umpire_adjudication(&adjudication, &args, &log.state, scenario.rules())?;
    record_standing(&mut log, &adjudication.standing, None)?;
    for arg in &args {
        let r = adjudication
            .rulings
            .iter()
            .find(|r| r.argument_id == arg.id)
            .ok_or("Urteil fehlt")?;
        resolve_argument(&mut log, &scenario, arg, r, None)?;
    }
    cursor = finish_round(&mut log, &loaded, cursor, &cfg)?;
    Ok(RoundOne {
        loaded,
        log,
        cursor,
        cfg,
        args,
    })
}

/// Spielt Runde 2 (Rat beruft sich auf Polizei → Präzedenzfall; Verdacht
/// erreicht die Spitze) und beendet das Spiel. Liefert zusätzlich den
/// Adjudikations-Prompt des Umpires aus Runde 2.
fn full_game(master: &[u8; 32]) -> Fixture<(LoadedScenario, GameLog, String)> {
    let RoundOne {
        loaded,
        mut log,
        cursor,
        cfg,
        ..
    } = round_one(master)?;
    let scenario = loaded.scenario.clone();
    let mut cursor = begin_round(&mut log, &loaded, cursor, &cfg)?;
    let mut sealed = ArgumentBox::new(cursor.round);
    sealed.seal(
        PlayerId::new("rat"),
        argument(
            "Die Inselpolizei übernimmt den Schutz der Baustelle der Notleitung.",
            &[
                "Die Polizei ist vor Ort.",
                "Die Baustelle liegt nahe der Wache.",
            ],
            false,
            None,
        ),
    )?;
    sealed.forfeit(PlayerId::new("gilde"))?;
    sealed.seal(
        PlayerId::new("nord"),
        argument(
            "Das Nordreich schickt Techniker zur Anlage.",
            &["Die Techniker warten im Nordhafen."],
            false,
            None,
        ),
    )?;
    sealed.forfeit(PlayerId::new("mission"))?;
    let mut args = reveal_round(&mut log, &scenario, sealed, None)?;

    cursor = cursor.next(&cfg).ok_or("FSM")?;
    enter_phase(&mut log, cursor, None)?;
    for p in scenario.seat_order() {
        submit_counters(&mut log, &mut args, &p, &CounterArgument::default(), None)?;
    }
    submit_red_cell(
        &mut log,
        &scenario,
        &mut args,
        &RedCellObjection {
            no_objection: true,
            ..RedCellObjection::default()
        },
        None,
    )?;

    cursor = cursor.next(&cfg).ok_or("FSM")?;
    enter_phase(&mut log, cursor, None)?;
    let umpire_prompt = prompt(
        &loaded,
        &log,
        &Seat::Umpire,
        ContractKind::UmpireAdjudication,
    );
    let mut r1 = ruling("r2-a1");
    r1.pro_weights = vec![1, 1];
    r1.public_rationale = Some("Wie in Runde 1: Leute vor Ort.".to_owned());
    let raise = vec![EffectOp::RaiseSuspicion {
        secret_id: "s1".to_owned(),
        by: 2,
    }];
    r1.on_success.clone_from(&raise);
    r1.on_failure = raise;
    let mut r3 = ruling("r2-a3");
    r3.pro_weights = vec![1];
    let adjudication = UmpireAdjudication {
        rulings: vec![r1, r3],
        conflicts: Vec::new(),
        standing: Vec::new(),
    };
    validate_umpire_adjudication(&adjudication, &args, &log.state, scenario.rules())?;
    for arg in &args {
        let r = adjudication
            .rulings
            .iter()
            .find(|r| r.argument_id == arg.id)
            .ok_or("Urteil fehlt")?;
        resolve_argument(&mut log, &scenario, arg, r, None)?;
    }
    finish_round(&mut log, &loaded, cursor, &cfg)?;
    end_game(&mut log, "Testende", None)?;
    Ok((loaded, log, umpire_prompt))
}

fn player_seats() -> Vec<Seat> {
    ORDER.iter().map(|p| Seat::player(*p)).collect()
}

#[test]
fn behavior_profile_is_private_and_reminded_every_turn() -> Fixture<()> {
    let RoundOne { loaded, log, .. } = round_one(&[11u8; 32])?;
    let rat = Seat::player("rat");
    let sys = system_prompt(SeatRole::Player, &loaded, Some(&rat));
    assert!(sys.contains("## Dein Verhaltensprofil (vertraulich)"));
    assert!(sys.contains(RAT_RED_LINE));
    assert!(sys.contains("Die Wahl in drei Monaten"));
    assert!(sys.contains("Verluste wiegen für sie schwerer"));
    assert!(sys.contains("vorsichtig (0.25"));
    for other in [
        Seat::player("gilde"),
        Seat::player("nord"),
        Seat::Umpire,
        Seat::RedCell,
    ] {
        let role = SeatRole::for_seat(&loaded, &other);
        let text = system_prompt(role, &loaded, Some(&other));
        assert!(
            !text.contains(RAT_RED_LINE),
            "{other}: fremdes Profil im System-Prompt"
        );
    }
    // Gilde hat ein eigenes Profil ohne Anker, Nordreich keins.
    let gilde_sys = system_prompt(SeatRole::Player, &loaded, Some(&Seat::player("gilde")));
    assert!(gilde_sys.contains("Den Zoll nie offen angreifen"));
    let nord_sys = system_prompt(SeatRole::Player, &loaded, Some(&Seat::player("nord")));
    assert!(!nord_sys.contains("Verhaltensprofil"));

    // Erinnerung in jedem Zug-Prompt des eigenen Sitzes — auch im reinen Delta.
    let rat_view = view(&loaded, &log, &rat);
    let n = rat_view.entries().len();
    let call = ExpectedCall {
        seat: rat.clone(),
        contract: ContractKind::PlayerArgument,
    };
    for (since, report) in [(0, true), (n, false)] {
        let p = turn_prompt(&rat_view, since, &call, report);
        assert!(p.contains("## Erinnerung: dein Verhaltensprofil"));
        assert!(p.contains(RAT_RED_LINE));
    }
    for seat in [
        Seat::player("gilde"),
        Seat::player("nord"),
        Seat::player("mission"),
    ] {
        let p = prompt(&loaded, &log, &seat, ContractKind::PlayerArgument);
        assert!(
            !p.contains(RAT_RED_LINE),
            "{seat}: fremdes Profil im Zug-Prompt"
        );
    }
    for (seat, contract) in [
        (Seat::Umpire, ContractKind::UmpireAdjudication),
        (Seat::RedCell, ContractKind::RedCellObjection),
    ] {
        let p = prompt(&loaded, &log, &seat, contract);
        assert!(!p.contains(RAT_RED_LINE), "{seat}: Profil im Zug-Prompt");
        assert!(!view(&loaded, &log, &seat).contains_text(RAT_RED_LINE)?);
    }
    let gilde_prompt = prompt(
        &loaded,
        &log,
        &Seat::player("gilde"),
        ContractKind::PlayerArgument,
    );
    assert!(gilde_prompt.contains("## Erinnerung: dein Verhaltensprofil"));
    let nord_prompt = prompt(
        &loaded,
        &log,
        &Seat::player("nord"),
        ContractKind::PlayerArgument,
    );
    assert!(!nord_prompt.contains("## Erinnerung"));
    Ok(())
}

fn invalid(src: &str) -> Vec<String> {
    match load_scenario(src) {
        Err(MatrixError::ScenarioInvalid(errs)) => errs,
        other => vec![format!("UNERWARTET: {other:?}")],
    }
}

#[test]
fn extension_validation() -> Fixture<()> {
    let ext = karst_extended()?;
    let loaded = load_scenario(&ext)?;
    assert!(loaded.scenario.red_cell().is_some());
    assert_eq!(loaded.scenario.inject_packages().len(), 2);
    assert!(loaded.scenario.behavior_of(&PlayerId::new("rat")).is_some());
    assert!(load_scenario(KARST)?.scenario.red_cell().is_none());

    let one_line = ext.replacen(
        "red_lines = [\"Den Zoll nie offen angreifen\", \"Keine Tanker verkaufen\"]",
        "red_lines = [\"Den Zoll nie offen angreifen\"]",
        1,
    );
    assert!(
        invalid(&one_line)
            .iter()
            .any(|e| e.contains("behavior.red_lines"))
    );
    let risky = ext.replacen("risk = 0.8", "risk = 1.5", 1);
    assert!(invalid(&risky).iter().any(|e| e.contains("behavior.risk")));
    let sharp = ext.replacen("sharpness = 0.7", "sharpness = 2.0", 1);
    assert!(
        invalid(&sharp)
            .iter()
            .any(|e| e.contains("red_cell.sharpness"))
    );
    let three = ext.replacen(
        "argument_system = \"pros_cons\"",
        "argument_system = \"three_reasons\"",
        1,
    );
    assert!(
        invalid(&three)
            .iter()
            .any(|e| e.contains("red_cell braucht"))
    );
    // Deaktivierte Red Cell stört three_reasons nicht.
    let off = three.replacen("enabled = true\nsharpness", "enabled = false\nsharpness", 1);
    assert!(load_scenario(&off).is_ok());
    let reserved = KARST.replace("\"rat\"", "\"red_cell\"");
    assert!(invalid(&reserved).iter().any(|e| e.contains("reserviert")));
    let dup = ext.replacen("id = \"sturm-regen\"", "id = \"hitzewelle\"", 1);
    assert!(invalid(&dup).iter().any(|e| e.contains("ID doppelt")));
    let window = ext.replacen("earliest = 3", "earliest = 3\nlatest = 9", 1);
    assert!(invalid(&window).iter().any(|e| e.contains("Rundenfenster")));
    let too_many = ext.replacen("max_injects = 2", "max_injects = 4", 1);
    assert!(invalid(&too_many).iter().any(|e| e.contains("max_injects")));
    Ok(())
}

#[test]
fn red_cell_sees_only_public_and_its_cons_are_weighted() -> Fixture<()> {
    let RoundOne {
        loaded, log, args, ..
    } = round_one(&[12u8; 32])?;
    let red = Seat::RedCell;
    let role = SeatRole::for_seat(&loaded, &red);
    assert_eq!(role, SeatRole::RedCell);
    assert_eq!(role.agent_role(), "matrix-redcell");
    let sys = system_prompt(role, &loaded, Some(&red));
    assert!(sys.contains("keine Siegbedingung"));
    assert!(sys.contains("`red_cell_objection`"));
    assert!(sys.contains("höchstens 3"));
    assert!(!sys.contains("`player_argument`"));
    assert!(
        !sys.contains("Die Gilde als Schuldige"),
        "geheimes Ziel im Red-Cell-Prompt"
    );
    let umpire_sys = system_prompt(SeatRole::Umpire, &loaded, Some(&Seat::Umpire));
    assert!(umpire_sys.contains("## Red Cell"));

    // Sicht: nur Öffentliches.
    let v = view(&loaded, &log, &red);
    for marker in [
        SECRET_ACTION,
        "Nur für uns",
        "Die Gilde als Schuldige der Krise dastehen lassen",
        "smuggling_net",
        "north_agents",
        "plant_sabotage_risk",
        "Riskant",
    ] {
        assert!(!v.contains_text(marker)?, "Red Cell sieht `{marker}`");
    }
    let p = prompt(&loaded, &log, &red, ContractKind::RedCellObjection);
    assert!(p.contains("Öffentliche Argumente: `r1-a1` (rat), `r1-a3` (nord)"));
    assert!(p.ends_with("Antworte ausschließlich mit JSON nach Vertrag `red_cell_objection`."));
    assert!(
        p.contains(ASSUMPTION),
        "eigener öffentlicher Einwand im Protokoll"
    );

    // Contras hängen unter `red_cell` am Ziel; fehlende Gewichte fallen auf.
    let a1 = args.iter().find(|a| a.id == "r1-a1").ok_or("r1-a1")?;
    assert!(a1.counters.contains_key(&PlayerId::new(RED_CELL_KEY)));
    let mut adjudication = round_one_adjudication();
    if let Some(r) = adjudication.rulings.first_mut() {
        r.con_weights.remove(RED_CELL_KEY);
    }
    let err =
        validate_umpire_adjudication(&adjudication, &args, &log.state, loaded.scenario.rules());
    assert!(
        matches!(err, Err(MatrixError::Contract(e)) if e.iter().any(|x| x.contains("red_cell")))
    );
    // `red_cell` ist kein Sitz für `standing`.
    let mut standing = round_one_adjudication();
    standing.standing = vec![RED_CELL_KEY.to_owned()];
    assert!(
        validate_umpire_adjudication(&standing, &args, &log.state, loaded.scenario.rules())
            .is_err()
    );

    // Contract-Regeln des Einwands.
    let settings = loaded.scenario.red_cell().ok_or("Red Cell fehlt")?;
    let ok = |o: &RedCellObjection| validate_red_cell_objection(o, &args, settings).is_ok();
    assert!(ok(&RedCellObjection {
        no_objection: true,
        ..RedCellObjection::default()
    }));
    assert!(!ok(&RedCellObjection {
        no_objection: true,
        target: Some("r1-a1".to_owned()),
        ..RedCellObjection::default()
    }));
    assert!(!ok(&RedCellObjection::default()), "leerer Einwand");
    let base = RedCellObjection {
        no_objection: false,
        target: Some("r1-a2".to_owned()),
        assumption: Some("x".to_owned()),
        cons: vec!["y".to_owned()],
    };
    assert!(!ok(&base), "geheimes Argument ist kein Ziel");
    let mut four = base.clone();
    four.target = Some("r1-a3".to_owned());
    assert!(ok(&four));
    four.cons = vec!["a".to_owned(); 4];
    assert!(!ok(&four), "mehr Contras als die Schärfe erlaubt");

    // Ohne aktive Red Cell kein Einreichen.
    let plain = load_scenario(KARST)?;
    let mut plain_log = open_game(&plain, &[1u8; 32], None)?;
    let mut no_args: Vec<RoundArgument> = Vec::new();
    assert!(matches!(
        submit_red_cell(&mut plain_log, &plain.scenario, &mut no_args, &base, None),
        Err(MatrixError::Phase(_))
    ));
    Ok(())
}

#[test]
fn suspicion_ladder_uses_templates_and_reveals_at_the_top() -> Fixture<()> {
    let RoundOne {
        loaded, log, args, ..
    } = round_one(&[13u8; 32])?;
    assert_eq!(log.state.suspicion_of("s1"), SuspicionLevel::Suspicion);
    let lines: Vec<(String, bool)> = log
        .journal
        .entries()
        .filter_map(|e| match &e.kind {
            EntryKind::SuspicionRaised { text, to, .. } => Some((
                text.clone(),
                e.audience.is_public() && *to == SuspicionLevel::Suspicion,
            )),
            _ => None,
        })
        .collect();
    assert_eq!(lines.len(), 1);
    let (line, public) = lines.first().cloned().ok_or("keine Zeile")?;
    assert!(public);
    assert_eq!(line, SuspicionLevel::Suspicion.public_line("s1"));
    for word in ["schleust", "Tanker", "Zoll", "nachts"] {
        assert!(
            !line.contains(word),
            "Geheiminhalt `{word}` in der Verdachtszeile"
        );
    }
    for seat in [Seat::player("rat"), Seat::player("nord"), Seat::RedCell] {
        let v = view(&loaded, &log, &seat);
        assert!(
            v.contains_text(&line)?,
            "{seat} sieht die Verdachtszeile nicht"
        );
        assert!(
            !v.contains_text(SECRET_ACTION)?,
            "{seat} sieht den Geheiminhalt"
        );
    }
    let rat_prompt = prompt(
        &loaded,
        &log,
        &Seat::player("rat"),
        ContractKind::PlayerArgument,
    );
    assert!(rat_prompt.contains("### Verdachtslage\n- #s1 Verdacht"));

    // Grenzen der Effekt-Op.
    let state = &log.state;
    let rules = loaded.scenario.rules();
    let mut adjudication = round_one_adjudication();
    if let Some(r) = adjudication.rulings.get_mut(1) {
        r.on_success = vec![EffectOp::RaiseSuspicion {
            secret_id: "s1".to_owned(),
            by: 1,
        }];
    }
    let err = validate_umpire_adjudication(&adjudication, &args, state, rules);
    assert!(
        matches!(err, Err(MatrixError::Contract(e)) if e.iter().any(|x| x.contains("geheime Argumente dürfen keine")))
    );
    let mut adjudication = round_one_adjudication();
    if let Some(r) = adjudication.rulings.first_mut() {
        r.on_success = vec![EffectOp::RaiseSuspicion {
            secret_id: "s1".to_owned(),
            by: 3,
        }];
        r.on_failure = vec![
            EffectOp::RaiseSuspicion {
                secret_id: "s1".to_owned(),
                by: 2,
            },
            EffectOp::RaiseSuspicion {
                secret_id: "s1".to_owned(),
                by: 1,
            },
        ];
    }
    let err = validate_umpire_adjudication(&adjudication, &args, state, rules);
    let Err(MatrixError::Contract(errors)) = err else {
        return Err("Grenzen nicht geprüft".into());
    };
    assert!(errors.iter().any(|e| e.contains("by = 3")));
    assert!(errors.iter().any(|e| e.contains("höchstens um 2 Stufen")));
    let mut adjudication = round_one_adjudication();
    if let Some(r) = adjudication.rulings.first_mut() {
        r.on_success = vec![EffectOp::RaiseSuspicion {
            secret_id: "s9".to_owned(),
            by: 1,
        }];
    }
    assert!(validate_umpire_adjudication(&adjudication, &args, state, rules).is_err());

    // Runde 2: Stufe 4 erreicht → Offenlegung mit Salt, für alle sichtbar.
    let (loaded, log, _) = full_game(&[13u8; 32])?;
    let revealed = log.journal.entries().find_map(|e| match &e.kind {
        EntryKind::SecretRevealed { secret_id, by, .. } if secret_id == "s1" => {
            Some((e.round, by.clone()))
        }
        _ => None,
    });
    assert_eq!(
        revealed,
        Some((2, RevealedBy::Suspicion("r2-a1".to_owned())))
    );
    assert_eq!(log.state.suspicion_of("s1"), SuspicionLevel::Revealed);
    assert!(view(&loaded, &log, &Seat::player("rat")).contains_text(SECRET_ACTION)?);
    assert!(view(&loaded, &log, &Seat::RedCell).contains_text(SECRET_ACTION)?);
    replay(&loaded.scenario, &log.journal)?;
    Ok(())
}

#[test]
fn precedents_are_public_recalled_and_listed() -> Fixture<()> {
    let RoundOne {
        loaded, log, args, ..
    } = round_one(&[14u8; 32])?;
    let set: Vec<_> = log
        .journal
        .entries()
        .filter_map(|e| match &e.kind {
            EntryKind::PrecedentSet { precedent } => Some((precedent.clone(), e.audience.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(set.len(), 1);
    let (p, audience) = set.first().cloned().ok_or("kein Präzedenzfall")?;
    assert!(audience.is_public());
    assert_eq!(p.id, "p1");
    assert_eq!(p.argument_id, "r1-a1");
    assert_eq!(p.tags, vec!["polizei".to_owned(), "schutz".to_owned()]);

    // Nur öffentliche Argumente.
    let mut adjudication = round_one_adjudication();
    if let Some(r) = adjudication.rulings.get_mut(1) {
        r.precedent = Some(PrecedentFlag {
            principle: "Geheimes bleibt geheim.".to_owned(),
            tags: vec!["zoll".to_owned()],
        });
    }
    let err =
        validate_umpire_adjudication(&adjudication, &args, &log.state, loaded.scenario.rules());
    assert!(
        matches!(err, Err(MatrixError::Contract(e)) if e.iter().any(|x| x.contains("nur für öffentliche")))
    );

    // Späterer Adjudikations-Prompt nennt den Maßstab.
    let (loaded, log, umpire_prompt) = full_game(&[14u8; 32])?;
    assert!(umpire_prompt.contains("Einschlägige Präzedenzfälle"));
    assert!(umpire_prompt.contains(PRINCIPLE));
    assert!(umpire_prompt.contains("→ betrifft `r2-a1`"));
    assert!(!umpire_prompt.contains("→ betrifft `r2-a3`"));
    // Spieler sehen den öffentlichen Präzedenzfall im Protokoll.
    assert!(view(&loaded, &log, &Seat::player("gilde")).contains_text(PRINCIPLE)?);

    let md = build_aar(&AarInput {
        loaded: &loaded,
        journal: &log.journal,
        state: &log.state,
        synthesis: None,
        debriefs: &BTreeMap::new(),
        models: None,
    })?;
    assert!(md.contains("## Präzedenzregister"));
    assert!(md.contains("| p1 | 1 | r1-a1 |"));
    assert!(md.contains("| r2-a1 |"), "späterer Fall fehlt:\n{md}");
    Ok(())
}

#[test]
fn full_extended_game_replays_and_aar_reports_everything() -> Fixture<()> {
    let (loaded, log, _) = full_game(&[15u8; 32])?;
    let replayed = replay(&loaded.scenario, &log.journal)?;
    assert_eq!(replayed.state_hash()?, log.state.state_hash()?);
    // Inject-Paket „unruhen“: Injects nie in derselben Runde.
    let applied: Vec<(u32, String)> = log
        .journal
        .entries()
        .filter_map(|e| match &e.kind {
            EntryKind::InjectApplied { inject_id, .. } if inject_id.starts_with("unruhen") => {
                Some((e.round, inject_id.clone()))
            }
            _ => None,
        })
        .collect();
    let mut rounds: Vec<u32> = applied.iter().map(|(r, _)| *r).collect();
    rounds.dedup();
    assert_eq!(rounds.len(), applied.len());

    let md = build_aar(&AarInput {
        loaded: &loaded,
        journal: &log.journal,
        state: &log.state,
        synthesis: None,
        debriefs: &BTreeMap::new(),
        models: None,
    })?;
    for needle in [
        "## Design-Lehren",
        "### Plausibilität der Würfel",
        "### Verdachtsleiter",
        "## Red Cell",
        "- r2 kein Einwand",
        "Contra (red_cell)",
        "Red Cell — angegriffene Kernannahme",
        "Inject-Paket `unruhen`",
        RAT_RED_LINE,
    ] {
        assert!(md.contains(needle), "AAR ohne `{needle}`");
    }
    // Spieler-Sichten während des Spiels blieben sauber: niemand außer dem
    // Rat sah dessen Profil, das Paket sieht kein Sitz.
    for seat in player_seats()
        .into_iter()
        .chain([Seat::Umpire, Seat::RedCell])
    {
        let v = view(&loaded, &log, &seat);
        assert!(
            !v.contains_text("inject_package_selected")?,
            "{seat} sieht die Paketwahl"
        );
        let own = seat == Seat::player("rat");
        assert_eq!(v.contains_text(RAT_RED_LINE)?, own, "{seat}");
    }
    Ok(())
}

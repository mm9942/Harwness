//! Vorabprüfung beim Wiederherstellen und begrenzte Persistenz-Kodierung
//! (PL-100 WP2.1, Review 4505196829).
//!
//! Kindmodul von `dispatcher`, damit die Tests `Dispatcher::restore_within`,
//! `DispatchState::encode_within` und `DispatchState::decode_within` mit
//! kleinen Grenzen aufrufen, die privaten Eintragstypen kodieren und
//! Zustände über den Höchstwerten direkt bauen können (das Dekodieren
//! liefert keine).
//!
//! Abgedeckt:
//! - Eintragsgrenze beim Dekodieren, je Tabelle und innerer Liste in voller
//!   Größe: genau der Höchstwert wird dekodiert (und besteht die
//!   Vorabprüfung von `restore`), beim Eintrag darüber bricht
//!   `from_json_bounded` mit „too many table entries" ab, obwohl dieser
//!   Eintrag selbst kein JSON ist (er wird also nicht einmal übersprungen,
//!   der Rest ebenso wenig geparst); die Marke lässt sich nicht aus der
//!   Eingabe vortäuschen, und `decode_failure` erkennt sie, ohne die
//!   Meldung zu kopieren;
//! - Zeichenvorrat vor serde: genau druckbares ASCII ohne Backslash und
//!   JSON-Leerraum sind erlaubt; ein Escape, das serde annähme, und rohe
//!   Bytes wie `0x7F` (der Fall aus dem Review) werden abgelehnt, auch vor
//!   einer Tabelle über ihrem Höchstwert; eingerücktes JSON und ein
//!   Zeilenende am Schluss bleiben lesbar;
//! - Höchstwerte der Konfiguration: `validate` nimmt jede Konstante an,
//!   lehnt eins mehr ab, und der Grund nennt genau diese Zahl;
//! - Vorabprüfung als Zahlen: jede Tabelle genau an ihrem absoluten
//!   Höchstwert gültig, eins mehr abgelehnt (ohne Einträge zu bauen, denn
//!   Zehntausende wären unnötig teuer);
//! - Reihenfolge über die Fassade, je Tabelle: ein Zustand mit einem Eintrag
//!   mehr als die Grenze, dessen Einträge **außerdem** ungültig sind (bei
//!   Tabellen ohne Eintragsprüfung ist ein ungültiger Grouper-Eintrag der
//!   Zeuge, den `restore` als erstes prüft). Der Fehler ist der
//!   Längenfehler; mit einer Grenze mehr kommt der Eintragsfehler, die
//!   Einträge sind also wirklich ungültig und wurden vorher nie angesehen.
//!   Ebenso für die inneren Längen (wartende Befunde je Cgroup, Mitglieder
//!   je Auftrag), über einen in der Crate gebauten Zustand;
//! - Bytegrenze: ein Byte über der Grenze wird nicht dekodiert (Nullbytes
//!   ergäben sonst `"json encoding"`), genau die Grenze wird dekodiert;
//!   `from_json_bounded` ist genau `decode_within` mit `MAX_STATE_BYTES`
//!   (ohne einen Puffer dieser Größe anzulegen); `to_json` schreibt nichts
//!   über der Grenze; Rundreise samt `restore`;
//! - Herleitung von `MAX_STATE_BYTES`: je Tabelle bleibt ein Eintrag mit den
//!   längsten Werten in seiner Schranke, der feste Anteil ebenso, und die
//!   Konstante ist die ungesättigte Summe (unter 256 MiB).

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{InFlight, PendingAsk, StarvedEpisodeKey, StateRepr, decode_failure};
use crate::bounds::{
    BREAKER_BYTES, BUDGET_ESTABLISHED_BYTES, BUDGET_PARTITION_BYTES, BUDGET_UID_PRIVILEGE_BYTES,
    BUDGET_UID_RESERVE_BYTES, IN_FLIGHT_JOB_BYTES, KNOWN_KEY_BYTES, PENDING_ASK_BYTES,
    QUEUED_JOB_BYTES, STARVED_EPISODE_BYTES, STATE_FIXED_BYTES, TRACKED_CGROUP_BYTES, TableSizes,
    check_state_bytes, check_state_encoding,
};
use crate::budget::{EstablishedPartition, TokenBucket};
use crate::dispatcher_tests::{
    NOW, decode, decode_result, digest, enqueued, finding, key, loopback, on_cgroup, roomy, uuid,
};
use crate::group::CgroupWindow;
use crate::job::QueuedTriage;
use crate::queue::QueueEntry;
use crate::serde_map::TOO_MANY_ENTRIES;
use crate::test_support::{TestError, TestResult, missing};
use crate::*;

// Der Wert unter `pointer`.
fn at(json: &Value, pointer: &str) -> TestResult<Value> {
    json.pointer(pointer)
        .cloned()
        .ok_or_else(missing("json pointer"))
}

// Ersetzt den Wert unter `pointer`.
fn set(json: &mut Value, pointer: &str, value: Value) -> TestResult {
    *json
        .pointer_mut(pointer)
        .ok_or_else(missing("json pointer"))? = value;
    Ok(())
}

// `count` Abwandlungen von `template` (Nummern ab 1) als JSON-Liste.
fn variants(
    template: &Value,
    count: u32,
    edit: impl Fn(&mut Value, u32) -> TestResult,
) -> TestResult<Value> {
    let mut items = Vec::new();
    for n in 1..=count {
        let mut item = template.clone();
        edit(&mut item, n)?;
        items.push(item);
    }
    Ok(Value::Array(items))
}

// Ein Zustand mit Einträgen in Warteschlange (Auftrag 2), vergebenen
// Aufträgen (Auftrag 1, Cgroup 7), bekannten Schlüsseln, Grouper (ein
// wartender Befund in Cgroup 7), Budget-Eimern, Erstsichtungen, Breakern und
// einer offenen Rückfrage, als JSON.
fn rich_state() -> TestResult<Value> {
    let mut dispatcher = Dispatcher::new(roomy())?;
    let first = dispatcher.offer(on_cgroup(1, RULE_EGRESS_FLOW, Severity::Medium, 7, 0), NOW);
    enqueued(&first).ok_or_else(missing("job 1"))?;
    dispatcher
        .next_triage(NOW)
        .ok_or_else(missing("in-flight job"))?;
    let second = dispatcher.offer(finding(2, RULE_EGRESS_FLOW, Severity::Medium), NOW);
    enqueued(&second).ok_or_else(missing("job 2"))?;
    let asked = dispatcher.offer(loopback(3), NOW);
    assert!(asked.contains(&DispatchAction::AskUser {
        finding_id: uuid(3)
    }));
    Ok(serde_json::to_value(dispatcher.snapshot())?)
}

// Ein inhaltsfreier Zustandsfehler.
fn invalid(what: &'static str) -> Option<DispatchError> {
    Some(DispatchError::InvalidState { what })
}

#[test]
fn test_validate_accepts_each_limit_and_rejects_one_more() -> TestResult {
    let base = DispatchConfig::default();
    let more = |n: usize| n.saturating_add(1);
    // (an der Grenze, eins darüber, Feld, Grenze)
    let cases = [
        (
            DispatchConfig {
                max_parallel: MAX_PARALLEL_LIMIT,
                ..base
            },
            DispatchConfig {
                max_parallel: more(MAX_PARALLEL_LIMIT),
                ..base
            },
            "max_parallel",
            MAX_PARALLEL_LIMIT,
        ),
        (
            DispatchConfig {
                queue_capacity: QUEUE_CAPACITY_LIMIT,
                ..base
            },
            DispatchConfig {
                queue_capacity: more(QUEUE_CAPACITY_LIMIT),
                ..base
            },
            "queue_capacity",
            QUEUE_CAPACITY_LIMIT,
        ),
        (
            DispatchConfig {
                max_tracked_cgroups: MAX_TRACKED_CGROUPS_LIMIT,
                ..base
            },
            DispatchConfig {
                max_tracked_cgroups: more(MAX_TRACKED_CGROUPS_LIMIT),
                ..base
            },
            "max_tracked_cgroups",
            MAX_TRACKED_CGROUPS_LIMIT,
        ),
        (
            DispatchConfig {
                max_known_keys: MAX_KNOWN_KEYS_LIMIT,
                ..base
            },
            DispatchConfig {
                max_known_keys: more(MAX_KNOWN_KEYS_LIMIT),
                ..base
            },
            "max_known_keys",
            MAX_KNOWN_KEYS_LIMIT,
        ),
        (
            DispatchConfig {
                budget: BudgetConfig {
                    max_partitions: BUDGET_MAX_PARTITIONS_LIMIT,
                    ..base.budget
                },
                ..base
            },
            DispatchConfig {
                budget: BudgetConfig {
                    max_partitions: more(BUDGET_MAX_PARTITIONS_LIMIT),
                    ..base.budget
                },
                ..base
            },
            "budget.max_partitions",
            BUDGET_MAX_PARTITIONS_LIMIT,
        ),
        (
            DispatchConfig {
                breaker: BreakerConfig {
                    max_partitions: BREAKER_MAX_PARTITIONS_LIMIT,
                    ..base.breaker
                },
                ..base
            },
            DispatchConfig {
                breaker: BreakerConfig {
                    max_partitions: more(BREAKER_MAX_PARTITIONS_LIMIT),
                    ..base.breaker
                },
                ..base
            },
            "breaker.max_partitions",
            BREAKER_MAX_PARTITIONS_LIMIT,
        ),
    ];
    for (at_limit, beyond, field, limit) in cases {
        assert_eq!(at_limit.validate(), Ok(()), "{field}");
        let Some(DispatchError::InvalidConfig {
            field: rejected,
            reason,
        }) = beyond.validate().err()
        else {
            return Err(TestError::Unexpected(format!(
                "{field}: accepted beyond its limit"
            )));
        };
        assert_eq!(rejected, field);
        // Der Grund nennt genau die Konstante.
        assert!(reason.ends_with(&format!(" {limit}")), "{field}: {reason}");
    }
    Ok(())
}

#[test]
fn test_table_sizes_accept_each_legal_maximum_and_reject_one_more() -> TestResult {
    let max = TableSizes::LEGAL_MAX;
    // Die Höchstwerte sind die öffentlichen Konstanten.
    assert_eq!(max.in_flight_jobs, MAX_PARALLEL_LIMIT);
    assert_eq!(max.queued_jobs, QUEUE_CAPACITY_LIMIT);
    assert_eq!(max.known_keys, MAX_KNOWN_KEYS_LIMIT);
    assert_eq!(max.tracked_cgroups, MAX_TRACKED_CGROUPS_LIMIT);
    assert_eq!(max.budget_partitions, BUDGET_MAX_PARTITIONS_LIMIT);
    assert_eq!(max.budget_uid_reserves, BUDGET_MAX_PARTITIONS_LIMIT);
    assert_eq!(max.budget_established, BUDGET_MAX_PARTITIONS_LIMIT);
    assert_eq!(max.budget_uid_privileges, BUDGET_MAX_PARTITIONS_LIMIT);
    assert_eq!(max.breakers, BREAKER_MAX_PARTITIONS_LIMIT);
    assert_eq!(max.pending_asks, MAX_PENDING_ASKS);
    assert_eq!(max.starved_episodes, MAX_STARVED_EPISODES);
    // Jede Tabelle genau am Höchstwert ist gültig …
    assert_eq!(max.check_within(&max), Ok(()));
    // … eins mehr nicht, je Tabelle mit eigenem Fehler.
    let more = |n: usize| n.saturating_add(1);
    let cases = [
        (
            TableSizes {
                in_flight_jobs: more(max.in_flight_jobs),
                ..max
            },
            "too many in-flight jobs",
        ),
        (
            TableSizes {
                queued_jobs: more(max.queued_jobs),
                ..max
            },
            "too many queued jobs",
        ),
        (
            TableSizes {
                known_keys: more(max.known_keys),
                ..max
            },
            "too many known keys",
        ),
        (
            TableSizes {
                tracked_cgroups: more(max.tracked_cgroups),
                ..max
            },
            "too many tracked cgroups",
        ),
        (
            TableSizes {
                budget_partitions: more(max.budget_partitions),
                ..max
            },
            "too many budget partitions",
        ),
        (
            TableSizes {
                budget_uid_reserves: more(max.budget_uid_reserves),
                ..max
            },
            "too many budget uid reserves",
        ),
        (
            TableSizes {
                budget_established: more(max.budget_established),
                ..max
            },
            "too many established partitions",
        ),
        (
            TableSizes {
                budget_uid_privileges: more(max.budget_uid_privileges),
                ..max
            },
            "too many budget uid privileges",
        ),
        (
            TableSizes {
                breakers: more(max.breakers),
                ..max
            },
            "too many breakers",
        ),
        (
            TableSizes {
                pending_asks: more(max.pending_asks),
                ..max
            },
            "too many pending asks",
        ),
        (
            TableSizes {
                starved_episodes: more(max.starved_episodes),
                ..max
            },
            "too many starved episodes",
        ),
    ];
    for (sizes, what) in cases {
        assert_eq!(
            sizes.check_within(&max),
            Err(DispatchError::InvalidState { what }),
            "{what}"
        );
    }
    Ok(())
}

// Eine Tabelle mit drei Einträgen für die Reihenfolge-Prüfung.
struct Oversized {
    // JSON-Zeiger auf die Tabelle.
    pointer: &'static str,
    // Drei Einträge; ungültig, wo die Tabelle eine Eintragsprüfung hat.
    entries: Value,
    // Die Tabelle hat keine Eintragsprüfung: ein ungültiger wartender
    // Befund im Grouper (die erste Eintragsprüfung von `restore`) ist der
    // Zeuge.
    witness: bool,
    // Die absoluten Höchstwerte, nur diese Tabelle mit Grenze `n`.
    bounds: fn(usize) -> TableSizes,
    // Fehler bei Grenze 2 (Längenfehler).
    too_many: &'static str,
    // Fehler bei Grenze 3 (Eintragsfehler).
    entry_error: &'static str,
}

#[test]
fn test_restore_rejects_each_oversized_table_before_any_entry_check() -> TestResult {
    let config = roomy();
    let json = rich_state()?;
    let bad_sensor = || Value::from("bad sensor");
    let kid = |n: u32| Value::from(u64::from(n).saturating_add(100));
    // Bekannte Schlüssel sind ein JSON-Objekt; drei ungültige Namen.
    let known_entry = json
        .pointer("/known/keys")
        .and_then(Value::as_object)
        .and_then(|keys| keys.values().next())
        .cloned()
        .ok_or_else(missing("known key"))?;
    let mut bad_keys = serde_json::Map::new();
    for n in 1..=3_u32 {
        bad_keys.insert(format!("not-a-key-{n}"), known_entry.clone());
    }
    let partition_edit = |pair: &mut Value, n: u32| -> TestResult {
        set(pair, "/0/cgroup_kid", kid(n))?;
        set(pair, "/0/sensor", bad_sensor())
    };
    let cases = [
        Oversized {
            pointer: "/in_flight",
            entries: variants(&at(&json, "/in_flight/0")?, 3, |flight, _| {
                set(flight, "/job/job_id", Value::from(0_u64))
            })?,
            witness: false,
            bounds: |n| TableSizes {
                in_flight_jobs: n,
                ..TableSizes::LEGAL_MAX
            },
            too_many: "too many in-flight jobs",
            entry_error: "triage jobs",
        },
        Oversized {
            pointer: "/queue/entries",
            entries: variants(&at(&json, "/queue/entries/0")?, 3, |entry, n| {
                set(entry, "/item/job_id", Value::from(0_u64))?;
                set(entry, "/seq", Value::from(n))
            })?,
            witness: false,
            bounds: |n| TableSizes {
                queued_jobs: n,
                ..TableSizes::LEGAL_MAX
            },
            too_many: "too many queued jobs",
            entry_error: "triage jobs",
        },
        Oversized {
            pointer: "/known/keys",
            entries: Value::Object(bad_keys),
            witness: false,
            bounds: |n| TableSizes {
                known_keys: n,
                ..TableSizes::LEGAL_MAX
            },
            too_many: "too many known keys",
            entry_error: "known keys",
        },
        Oversized {
            pointer: "/grouper/cgroups",
            entries: variants(&at(&json, "/grouper/cgroups/0")?, 3, |pair, n| {
                set(pair, "/0", kid(n))?;
                set(pair, "/1/entries/0/finding_id", Value::from("not-a-uuid"))
            })?,
            witness: false,
            bounds: |n| TableSizes {
                tracked_cgroups: n,
                ..TableSizes::LEGAL_MAX
            },
            too_many: "too many tracked cgroups",
            entry_error: "incident grouper",
        },
        Oversized {
            pointer: "/budget/partitions",
            entries: variants(&at(&json, "/budget/partitions/0")?, 3, partition_edit)?,
            witness: false,
            bounds: |n| TableSizes {
                budget_partitions: n,
                ..TableSizes::LEGAL_MAX
            },
            too_many: "too many budget partitions",
            entry_error: "budget",
        },
        Oversized {
            pointer: "/budget/uid_reserve",
            entries: variants(
                &json!([0, {"units": 0, "last_refill_unix": null, "starved": false}]),
                3,
                |pair, n| set(pair, "/0", Value::from(n)),
            )?,
            witness: true,
            bounds: |n| TableSizes {
                budget_uid_reserves: n,
                ..TableSizes::LEGAL_MAX
            },
            too_many: "too many budget uid reserves",
            entry_error: "incident grouper",
        },
        Oversized {
            pointer: "/budget/established",
            entries: variants(&at(&json, "/budget/established/0")?, 3, partition_edit)?,
            witness: false,
            bounds: |n| TableSizes {
                budget_established: n,
                ..TableSizes::LEGAL_MAX
            },
            too_many: "too many established partitions",
            entry_error: "budget",
        },
        Oversized {
            pointer: "/budget/uid_privileged",
            entries: variants(&json!([0, NOW]), 3, |pair, n| {
                set(pair, "/0", Value::from(n))
            })?,
            witness: true,
            bounds: |n| TableSizes {
                budget_uid_privileges: n,
                ..TableSizes::LEGAL_MAX
            },
            too_many: "too many budget uid privileges",
            entry_error: "incident grouper",
        },
        Oversized {
            pointer: "/breakers/partitions",
            entries: variants(&at(&json, "/breakers/partitions/0")?, 3, partition_edit)?,
            witness: false,
            bounds: |n| TableSizes {
                breakers: n,
                ..TableSizes::LEGAL_MAX
            },
            too_many: "too many breakers",
            entry_error: "breakers",
        },
        Oversized {
            pointer: "/pending_asks",
            entries: variants(&at(&json, "/pending_asks/0")?, 3, |pair, n| {
                // Kennung der Rückfrage ≠ Kennung ihres Befunds.
                set(pair, "/0", Value::from(format!("not-a-uuid-{n}")))
            })?,
            witness: false,
            bounds: |n| TableSizes {
                pending_asks: n,
                ..TableSizes::LEGAL_MAX
            },
            too_many: "too many pending asks",
            entry_error: "pending asks",
        },
        Oversized {
            pointer: "/starved_episodes",
            entries: variants(&json!([["breaker", 0], NOW]), 3, |pair, n| {
                set(pair, "/0/1", Value::from(n))
            })?,
            witness: true,
            bounds: |n| TableSizes {
                starved_episodes: n,
                ..TableSizes::LEGAL_MAX
            },
            too_many: "too many starved episodes",
            entry_error: "incident grouper",
        },
    ];
    for case in cases {
        let mut tampered = json.clone();
        set(&mut tampered, case.pointer, case.entries)?;
        if case.witness {
            set(
                &mut tampered,
                "/grouper/cgroups/0/1/entries/0/finding_id",
                Value::from("not-a-uuid"),
            )?;
        }
        let state = decode(&tampered)?;
        // Grenze 2: der Längenfehler, obwohl die Einträge ungültig sind …
        assert_eq!(
            Dispatcher::restore_within(config, state.clone(), &(case.bounds)(2)).err(),
            invalid(case.too_many),
            "{}",
            case.pointer
        );
        // … Grenze 3: dieselben Einträge werden geprüft und abgelehnt.
        assert_eq!(
            Dispatcher::restore_within(config, state, &(case.bounds)(3)).err(),
            invalid(case.entry_error),
            "{}",
            case.pointer
        );
    }
    Ok(())
}

#[test]
fn test_restore_rejects_oversized_cgroups_and_jobs_before_any_entry_check() -> TestResult {
    let config = roomy();
    let json = rich_state()?;
    let too_many_entries = invalid("too many table entries");
    // `count` ungültige wartende Befunde in Cgroup 7, als JSON.
    let mut bad_entry = at(&json, "/grouper/cgroups/0/1/entries/0")?;
    set(&mut bad_entry, "/finding_id", Value::from("not-a-uuid"))?;
    let with_entries = |count: usize| -> TestResult<Value> {
        let mut tampered = json.clone();
        set(
            &mut tampered,
            "/grouper/cgroups/0/1/entries",
            Value::Array(vec![bad_entry.clone(); count]),
        )?;
        Ok(tampered)
    };
    // Genau die Grenze wird dekodiert; dann scheitert die Eintragsprüfung,
    // die Einträge sind also wirklich ungültig.
    let at_limit = decode(&with_entries(MAX_ENTRIES_PER_CGROUP)?)?;
    assert_eq!(
        Dispatcher::restore(config, at_limit.clone()).err(),
        invalid("incident grouper")
    );
    // Eine mehr lehnt schon das Dekodieren ab …
    assert_eq!(
        decode_result(&with_entries(MAX_ENTRIES_PER_CGROUP.saturating_add(1))?)?.err(),
        too_many_entries
    );
    // … und `restore` einen so gebauten Zustand mit dem Längenfehler, bevor
    // ein Eintrag geprüft wird.
    let mut over = at_limit;
    let entries = over
        .grouper
        .entries_mut_for_test(7)
        .ok_or_else(missing("cgroup 7"))?;
    let extra = entries
        .first()
        .cloned()
        .ok_or_else(missing("waiting entry"))?;
    entries.push(extra);
    assert_eq!(
        Dispatcher::restore(config, over).err(),
        invalid("too many cgroup entries")
    );
    // `count` ungültige Mitglieder im wartenden bzw. vergebenen Auftrag.
    let mut bad_member = at(&json, "/queue/entries/0/item/members/0")?;
    set(
        &mut bad_member,
        "/record_digest",
        Value::from("not-a-digest"),
    )?;
    let with_members = |pointer: &str, count: usize| -> TestResult<Value> {
        let mut tampered = json.clone();
        set(
            &mut tampered,
            pointer,
            Value::Array(vec![bad_member.clone(); count]),
        )?;
        Ok(tampered)
    };
    let queued = "/queue/entries/0/item/members";
    let in_flight = "/in_flight/0/job/members";
    for pointer in [queued, in_flight] {
        assert_eq!(
            Dispatcher::restore(config, decode(&with_members(pointer, MAX_JOB_MEMBERS)?)?).err(),
            invalid("triage jobs"),
            "{pointer}"
        );
        assert_eq!(
            decode_result(&with_members(pointer, MAX_JOB_MEMBERS.saturating_add(1))?)?.err(),
            too_many_entries,
            "{pointer}"
        );
    }
    // In der Crate gebaut: ein ungültiges Mitglied mehr ergibt den
    // Längenfehler, nicht den Eintragsfehler.
    let mut queued_over = decode(&with_members(queued, MAX_JOB_MEMBERS)?)?;
    let entry = queued_over
        .queue
        .entries
        .first_mut()
        .ok_or_else(missing("queued job"))?;
    let extra = entry
        .item
        .members
        .first()
        .cloned()
        .ok_or_else(missing("member"))?;
    entry.item.members.push(extra);
    assert_eq!(
        Dispatcher::restore(config, queued_over).err(),
        invalid("too many job members")
    );
    let mut in_flight_over = decode(&with_members(in_flight, MAX_JOB_MEMBERS)?)?;
    let flight = in_flight_over
        .in_flight
        .first_mut()
        .ok_or_else(missing("in-flight job"))?;
    let extra = flight
        .job
        .members
        .first()
        .cloned()
        .ok_or_else(missing("member"))?;
    flight.job.members.push(extra);
    assert_eq!(
        Dispatcher::restore(config, in_flight_over).err(),
        invalid("too many job members")
    );
    Ok(())
}

#[test]
fn test_from_json_bounded_applies_the_byte_cap_before_parsing() -> TestResult {
    // Nullbytes sind kein JSON: würde dekodiert, käme `"json encoding"`.
    // Die Kante mit kleiner Grenze: genau `max` Bytes werden dekodiert (und
    // scheitern hier am Inhalt), eins mehr wird nicht dekodiert.
    assert_eq!(
        DispatchState::decode_within(&[0_u8; 8], 8).err(),
        invalid("json encoding")
    );
    assert_eq!(
        DispatchState::decode_within(&[0_u8; 9], 8).err(),
        invalid("too many bytes")
    );
    // Die öffentliche Grenze, ohne einen Puffer dieser Größe anzulegen …
    assert_eq!(check_state_bytes(MAX_STATE_BYTES, MAX_STATE_BYTES), Ok(()));
    assert_eq!(
        check_state_bytes(MAX_STATE_BYTES.saturating_add(1), MAX_STATE_BYTES).err(),
        invalid("too many bytes")
    );
    // … und `from_json_bounded` ist genau `decode_within` mit ihr.
    let bytes = Dispatcher::new(roomy())?.snapshot().to_json()?;
    let zeros = [0_u8; 9];
    for input in [
        bytes.as_slice(),
        b"".as_slice(),
        b"{\"version\":".as_slice(),
        zeros.as_slice(),
    ] {
        assert_eq!(
            DispatchState::from_json_bounded(input),
            DispatchState::decode_within(input, MAX_STATE_BYTES)
        );
    }
    Ok(())
}

#[test]
fn test_to_json_round_trips_and_never_writes_over_the_cap() -> TestResult {
    let state = decode(&rich_state()?)?;
    let bytes = state.to_json()?;
    // Kompakt, und `from_json_bounded` liest genau das zurück.
    assert_eq!(bytes, serde_json::to_vec(&state)?);
    assert_eq!(DispatchState::from_json_bounded(&bytes)?, state);
    // Genau an der Grenze wird gelesen und geschrieben, ein Byte darunter
    // nicht.
    let len = bytes.len();
    assert_eq!(DispatchState::decode_within(&bytes, len)?, state);
    assert_eq!(
        DispatchState::decode_within(&bytes, len.saturating_sub(1)).err(),
        invalid("too many bytes")
    );
    assert_eq!(state.encode_within(len)?, bytes);
    assert_eq!(
        state.encode_within(len.saturating_sub(1)).err(),
        invalid("too many bytes")
    );
    // Ungültige Kodierungen bleiben inhaltsfrei.
    assert_eq!(
        DispatchState::from_json_bounded(b"{\"version\":\"\\u202e\"}").err(),
        invalid("json encoding")
    );
    // Der gelesene Zustand läuft durch `restore`.
    let restored = Dispatcher::restore(roomy(), DispatchState::from_json_bounded(&bytes)?)?;
    assert_eq!(restored.pending_asks_len(), 1);
    assert_eq!(restored.queue_len(), 2);
    Ok(())
}

// Länge der kompakten Kodierung von `value`, gelesen als `T`. Die
// Rundreise muss `value` unverändert ergeben: ein vertipptes oder fehlendes
// Feld fiele sonst still weg, und der Eintrag wäre nicht der längste.
fn encoded_len<T>(value: &Value) -> TestResult<usize>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let typed: T = serde_json::from_value(value.clone())?;
    assert_eq!(&serde_json::to_value(&typed)?, value);
    Ok(serde_json::to_vec(&typed)?.len())
}

// Die Variante mit der längsten Kodierung.
fn longest_variant<T: serde::Serialize>(values: &[T]) -> TestResult<Value> {
    let mut best: Option<(usize, Value)> = None;
    for value in values {
        let json = serde_json::to_value(value)?;
        let len = json.to_string().len();
        if best.as_ref().is_none_or(|(best_len, _)| len > *best_len) {
            best = Some((len, json));
        }
    }
    best.map(|(_, json)| json)
        .ok_or_else(missing("enum values"))
}

// Die längsten Werte je Feldart eines gültigen Zustands.
struct Longest {
    // Schwere.
    severity: Value,
    // Härte.
    hardness: Value,
    // Triage-Rolle.
    role: Value,
    // Art des Befunds.
    kind: Value,
    // Ursache einer `Starved`-Episode.
    cause: Value,
    // Bekannte Regel.
    rule: &'static str,
    // Sensor-Kennung.
    sensor: String,
}

impl Longest {
    // Aus allen Varianten bzw. Grammatiken.
    fn new() -> TestResult<Self> {
        let rule = ALL_RULE_IDS
            .iter()
            .copied()
            .max_by_key(|rule| rule.len())
            .ok_or_else(missing("rule ids"))?;
        let sensor = "s".repeat(MAX_SENSOR_ID_LEN);
        assert!(is_sensor_id(&sensor));
        Ok(Self {
            severity: longest_variant(&[
                Severity::Info,
                Severity::Low,
                Severity::Medium,
                Severity::High,
                Severity::Critical,
            ])?,
            hardness: longest_variant(&[
                Hardness::Observed,
                Hardness::Correlated,
                Hardness::Inferred,
            ])?,
            role: longest_variant(&TriageRole::ALL)?,
            kind: longest_variant(&[FindingKind::RuleTriggered, FindingKind::Anomaly])?,
            cause: longest_variant(&[
                StarvedCause::Breaker,
                StarvedCause::PartitionBudget,
                StarvedCause::UidReserve,
                StarvedCause::GlobalBudget,
                StarvedCause::ReserveExhausted,
            ])?,
            rule,
            sensor,
        })
    }

    // Mitglied eines Auftrags.
    fn member(&self) -> Value {
        json!({
            "finding_id": uuid(u32::MAX),
            "dedup_key": key(u32::MAX),
            "rule_id": self.rule,
            "record_digest": digest(u32::MAX),
            "severity": self.severity.clone(),
            "hardness": self.hardness.clone(),
            "observed_at_unix": i64::MIN,
            "freeze_proposal_allowed": false,
        })
    }

    // Auftrag mit `MAX_JOB_MEMBERS` Mitgliedern.
    fn job(&self) -> Value {
        json!({
            "job_id": u64::MAX,
            "incident_id": u64::MAX,
            "incident_role": self.role.clone(),
            "members": vec![self.member(); MAX_JOB_MEMBERS],
            "novel": false,
            "retriage": false,
        })
    }

    // Fingerprint mit allen Feldern.
    fn fingerprint() -> Value {
        json!({
            "port": u16::MAX,
            "exe_digest": digest(u32::MAX),
            "rate_per_min": u64::MAX,
        })
    }

    // Rang eines Befunds im Grouper.
    fn rank(&self) -> Value {
        json!({
            "role": self.role.clone(),
            "severity": self.severity.clone(),
            "hardness": self.hardness.clone(),
            "observed_at_unix": i64::MIN,
        })
    }

    // Partition mit längstem Sensor.
    fn partition(&self) -> Value {
        json!({ "uid": u32::MAX, "cgroup_kid": u64::MAX, "sensor": self.sensor.as_str() })
    }

    // Token-Eimer.
    fn bucket() -> Value {
        json!({ "units": u64::MAX, "last_refill_unix": i64::MIN, "starved": false })
    }

    // Vollständiger Befund.
    fn finding(&self) -> Value {
        json!({
            "finding_id": uuid(u32::MAX),
            "rule_id": self.rule,
            "severity": self.severity.clone(),
            "hardness": self.hardness.clone(),
            "kind": self.kind.clone(),
            "dedup_key": key(u32::MAX),
            "cgroup_kid": u64::MAX,
            "uid": u32::MAX,
            "sensor": self.sensor.as_str(),
            "occurrences": u64::MAX,
            "observed_at_unix": i64::MIN,
            "scope_obfuscated": false,
            "record_digest": digest(u32::MAX),
            "loopback": false,
            "verified_source": false,
            "fingerprint": Self::fingerprint(),
        })
    }
}

// Prüft eine Schranke: Kodierung plus Komma.
fn assert_within(name: &str, len: usize, bound: usize) {
    let with_comma = len.saturating_add(1);
    assert!(with_comma <= bound, "{name}: {with_comma} > {bound}");
}

#[test]
fn test_longest_valid_entries_stay_within_their_byte_bounds() -> TestResult {
    let longest = Longest::new()?;
    // Die Grammatiken, von denen die Schranken ausgehen.
    assert_eq!(longest.rule.len(), 18);
    assert!(is_uuid_v4(&uuid(u32::MAX)));
    assert!(is_dedup_key(&key(u32::MAX)));
    assert!(is_record_digest(&digest(u32::MAX)));
    assert_within(
        "queued job",
        encoded_len::<QueueEntry<QueuedTriage>>(&json!({
            "priority": {
                "novel": false,
                "hardness": longest.hardness.clone(),
                "severity": longest.severity.clone(),
                "observed_at_unix": i64::MIN,
            },
            "seq": u64::MAX,
            "item": longest.job(),
        }))?,
        QUEUED_JOB_BYTES,
    );
    assert_within(
        "in-flight job",
        encoded_len::<InFlight>(&json!({
            "job": longest.job(),
            "dispatched_at_unix": i64::MIN,
        }))?,
        IN_FLIGHT_JOB_BYTES,
    );
    // Bekannte Schlüssel stehen als Name im JSON-Objekt: ohne dessen
    // Klammern.
    let mut known = serde_json::Map::new();
    known.insert(
        key(u32::MAX),
        json!({
            "fingerprint": Longest::fingerprint(),
            "hardness": longest.hardness.clone(),
            "last_seen_unix": i64::MIN,
        }),
    );
    assert_within(
        "known key",
        encoded_len::<BTreeMap<String, KnownFinding>>(&Value::Object(known))?.saturating_sub(2),
        KNOWN_KEY_BYTES,
    );
    let group_entry = json!({
        "rule_id": longest.rule,
        "finding_id": uuid(u32::MAX),
        "rank": longest.rank(),
    });
    assert_within(
        "tracked cgroup",
        encoded_len::<(u64, CgroupWindow)>(&json!([
            u64::MAX,
            {
                "entries": vec![group_entry; MAX_ENTRIES_PER_CGROUP],
                "incident": {
                    "incident_id": u64::MAX,
                    "primary": longest.rank(),
                    "last_observed_at_unix": i64::MIN,
                    "closes_at_unix": i64::MIN,
                    "job": u64::MAX,
                },
                "touched_unix": i64::MIN,
            }
        ]))?,
        TRACKED_CGROUP_BYTES,
    );
    assert_within(
        "budget partition",
        encoded_len::<(PartitionKey, TokenBucket)>(&json!([
            longest.partition(),
            Longest::bucket()
        ]))?,
        BUDGET_PARTITION_BYTES,
    );
    assert_within(
        "budget uid reserve",
        encoded_len::<(Option<u32>, TokenBucket)>(&json!([u32::MAX, Longest::bucket()]))?,
        BUDGET_UID_RESERVE_BYTES,
    );
    assert_within(
        "established partition",
        encoded_len::<(PartitionKey, EstablishedPartition)>(&json!([
            longest.partition(),
            {
                "first_seen_unix": i64::MIN,
                "last_seen_unix": i64::MIN,
                "last_privileged_unix": i64::MIN,
            }
        ]))?,
        BUDGET_ESTABLISHED_BYTES,
    );
    assert_within(
        "budget uid privilege",
        encoded_len::<(Option<u32>, i64)>(&json!([u32::MAX, i64::MIN]))?,
        BUDGET_UID_PRIVILEGE_BYTES,
    );
    assert_within(
        "breaker",
        encoded_len::<(PartitionKey, BreakerPartition)>(&json!([
            longest.partition(),
            {
                "window_start_unix": i64::MIN,
                "window_count": u32::MAX,
                "tripped": false,
                "quiet_start_unix": i64::MIN,
                "quiet_count": u32::MAX,
                "last_offer_unix": i64::MIN,
            }
        ]))?,
        BREAKER_BYTES,
    );
    assert_within(
        "pending ask",
        encoded_len::<(String, PendingAsk)>(&json!([
            uuid(u32::MAX),
            { "finding": longest.finding(), "asked_at_unix": i64::MIN }
        ]))?,
        PENDING_ASK_BYTES,
    );
    assert_within(
        "starved episode",
        encoded_len::<(StarvedEpisodeKey, i64)>(&json!([
            [longest.cause.clone(), u32::MAX],
            i64::MIN
        ]))?,
        STARVED_EPISODE_BYTES,
    );
    Ok(())
}

// Setzt jede Zahl in `value` auf `u64::MAX` (alle Zähler sind `u64`).
fn maximize_counters(value: &mut Value) {
    match value {
        Value::Number(_) => *value = Value::from(u64::MAX),
        Value::Object(fields) => fields.values_mut().for_each(maximize_counters),
        _ => {}
    }
}

#[test]
fn test_state_without_entries_stays_within_the_fixed_bound() -> TestResult {
    let mut fixed = serde_json::to_value(Dispatcher::new(roomy())?.snapshot())?;
    for pointer in [
        "/next_job",
        "/queue/next_seq",
        "/queue/evictions",
        "/grouper/next_incident",
    ] {
        set(&mut fixed, pointer, Value::from(u64::MAX))?;
    }
    set(&mut fixed, "/budget/global", Longest::bucket())?;
    set(
        &mut fixed,
        "/budget/reserve_since_unix",
        Value::from(i64::MIN),
    )?;
    set(&mut fixed, "/degraded_until_unix", Value::from(i64::MIN))?;
    maximize_counters(
        fixed
            .pointer_mut("/counters")
            .ok_or_else(missing("counters"))?,
    );
    // Rundreise über den Leseweg: kein Feld fällt still weg.
    let state = decode(&fixed)?;
    assert_eq!(serde_json::to_value(&state)?, fixed);
    let len = state.to_json()?.len();
    assert!(len <= STATE_FIXED_BYTES, "{len} > {STATE_FIXED_BYTES}");
    Ok(())
}

#[test]
fn test_max_state_bytes_is_the_unsaturated_sum_of_the_table_bounds() -> TestResult {
    let tables = [
        (QUEUE_CAPACITY_LIMIT, QUEUED_JOB_BYTES),
        (MAX_PARALLEL_LIMIT, IN_FLIGHT_JOB_BYTES),
        (MAX_KNOWN_KEYS_LIMIT, KNOWN_KEY_BYTES),
        (MAX_TRACKED_CGROUPS_LIMIT, TRACKED_CGROUP_BYTES),
        (BUDGET_MAX_PARTITIONS_LIMIT, BUDGET_PARTITION_BYTES),
        (BUDGET_MAX_PARTITIONS_LIMIT, BUDGET_UID_RESERVE_BYTES),
        (BUDGET_MAX_PARTITIONS_LIMIT, BUDGET_ESTABLISHED_BYTES),
        (BUDGET_MAX_PARTITIONS_LIMIT, BUDGET_UID_PRIVILEGE_BYTES),
        (BREAKER_MAX_PARTITIONS_LIMIT, BREAKER_BYTES),
        (MAX_PENDING_ASKS, PENDING_ASK_BYTES),
        (MAX_STARVED_EPISODES, STARVED_EPISODE_BYTES),
    ];
    let mut sum = Some(STATE_FIXED_BYTES);
    for (count, bytes) in tables {
        sum = sum.and_then(|total| {
            count
                .checked_mul(bytes)
                .and_then(|table| total.checked_add(table))
        });
    }
    // Ohne Überlauf gerechnet: die gesättigte Konstante ist also exakt.
    assert_eq!(sum, Some(MAX_STATE_BYTES));
    // Eine echte Grenze; wer einen Höchstwert hebt, entscheidet hier neu.
    assert!(sum.is_some_and(|total| total <= 256 << 20));
    Ok(())
}

// Platzhalter für die erzeugte Tabelle im kodierten Zustand.
const TABLE: &str = "@table@";

// Eine Tabelle für die Eintragsgrenze beim Dekodieren.
struct DecodeCap {
    // JSON-Zeiger auf die Tabelle im Zustand.
    pointer: &'static str,
    // Ihr absoluter Höchstwert.
    limit: usize,
    // Die Tabelle ist ein JSON-Objekt (sonst eine Liste).
    object: bool,
    // Der `n`-te Eintrag als kompaktes JSON (Schlüssel paarweise
    // verschieden).
    entry: Box<dyn Fn(usize) -> String>,
    // Die Länge der Tabelle im dekodierten Zustand.
    len: fn(&mut DispatchState) -> Option<usize>,
}

// Ein kodierter Zustand: `base`, dessen Tabelle unter `pointer` aus
// `count` Einträgen besteht, gefolgt von `tail` (roh, auch ungültiges
// JSON). Als Text gebaut: Zehntausende Einträge als `Value` wären unnötig
// teuer.
fn with_table(base: &Value, case: &DecodeCap, count: usize, tail: &str) -> TestResult<Vec<u8>> {
    let mut json = base.clone();
    set(&mut json, case.pointer, Value::from(TABLE))?;
    let text = serde_json::to_string(&json)?;
    let marker = format!("\"{TABLE}\"");
    let (head, rest) = text
        .split_once(marker.as_str())
        .ok_or_else(missing("table placeholder"))?;
    let (open, close) = if case.object { ("{", "}") } else { ("[", "]") };
    let mut out = String::new();
    out.push_str(head);
    out.push_str(open);
    for n in 0..count {
        if n > 0 {
            out.push(',');
        }
        out.push_str(&(case.entry)(n));
    }
    out.push_str(tail);
    out.push_str(close);
    out.push_str(rest);
    Ok(out.into_bytes())
}

// Kompaktes JSON des Werts unter `pointer`.
fn compact(json: &Value, pointer: &str) -> TestResult<String> {
    Ok(serde_json::to_string(&at(json, pointer)?)?)
}

// Eine Partition mit Cgroup `n` als kompaktes JSON.
fn partition_text(n: usize) -> String {
    format!(r#"{{"uid":null,"cgroup_kid":{n},"sensor":"s"}}"#)
}

#[test]
fn test_decoding_caps_every_table_at_its_legal_maximum() -> TestResult {
    let base = rich_state()?;
    let in_flight = compact(&base, "/in_flight/0")?;
    let queued = compact(&base, "/queue/entries/0")?;
    let ask = compact(&base, "/pending_asks/0/1")?;
    let group_entry = compact(&base, "/grouper/cgroups/0/1/entries/0")?;
    let member = compact(&base, "/queue/entries/0/item/members/0")?;
    let bucket = r#"{"units":0,"last_refill_unix":null,"starved":false}"#;
    let cases = [
        DecodeCap {
            pointer: "/in_flight",
            limit: MAX_PARALLEL_LIMIT,
            object: false,
            entry: Box::new(move |_| in_flight.clone()),
            len: |state| Some(state.table_sizes().in_flight_jobs),
        },
        DecodeCap {
            pointer: "/queue/entries",
            limit: QUEUE_CAPACITY_LIMIT,
            object: false,
            entry: Box::new(move |_| queued.clone()),
            len: |state| Some(state.table_sizes().queued_jobs),
        },
        DecodeCap {
            pointer: "/known/keys",
            limit: MAX_KNOWN_KEYS_LIMIT,
            object: true,
            entry: Box::new(|n| {
                format!(r#""k{n}":{{"fingerprint":{{}},"hardness":"observed","last_seen_unix":0}}"#)
            }),
            len: |state| Some(state.table_sizes().known_keys),
        },
        DecodeCap {
            pointer: "/grouper/cgroups",
            limit: MAX_TRACKED_CGROUPS_LIMIT,
            object: false,
            entry: Box::new(|n| {
                format!(r#"[{n},{{"entries":[],"incident":null,"touched_unix":0}}]"#)
            }),
            len: |state| Some(state.table_sizes().tracked_cgroups),
        },
        DecodeCap {
            pointer: "/budget/partitions",
            limit: BUDGET_MAX_PARTITIONS_LIMIT,
            object: false,
            entry: Box::new(move |n| format!("[{},{bucket}]", partition_text(n))),
            len: |state| Some(state.table_sizes().budget_partitions),
        },
        DecodeCap {
            pointer: "/budget/uid_reserve",
            limit: BUDGET_MAX_PARTITIONS_LIMIT,
            object: false,
            entry: Box::new(move |n| format!("[{n},{bucket}]")),
            len: |state| Some(state.table_sizes().budget_uid_reserves),
        },
        DecodeCap {
            pointer: "/budget/established",
            limit: BUDGET_MAX_PARTITIONS_LIMIT,
            object: false,
            entry: Box::new(|n| {
                format!(
                    r#"[{},{{"first_seen_unix":0,"last_seen_unix":0,"last_privileged_unix":null}}]"#,
                    partition_text(n)
                )
            }),
            len: |state| Some(state.table_sizes().budget_established),
        },
        DecodeCap {
            // Die Paare `[n,0]` aus dem Review: je 6 bis 13 Bytes.
            pointer: "/budget/uid_privileged",
            limit: BUDGET_MAX_PARTITIONS_LIMIT,
            object: false,
            entry: Box::new(|n| format!("[{n},0]")),
            len: |state| Some(state.table_sizes().budget_uid_privileges),
        },
        DecodeCap {
            pointer: "/breakers/partitions",
            limit: BREAKER_MAX_PARTITIONS_LIMIT,
            object: false,
            entry: Box::new(|n| {
                format!(
                    r#"[{},{{"window_start_unix":0,"window_count":0,"tripped":false,"quiet_start_unix":0,"quiet_count":0,"last_offer_unix":0}}]"#,
                    partition_text(n)
                )
            }),
            len: |state| Some(state.table_sizes().breakers),
        },
        DecodeCap {
            pointer: "/pending_asks",
            limit: MAX_PENDING_ASKS,
            object: false,
            entry: Box::new(move |n| format!(r#"["a{n}",{ask}]"#)),
            len: |state| Some(state.table_sizes().pending_asks),
        },
        DecodeCap {
            pointer: "/starved_episodes",
            limit: MAX_STARVED_EPISODES,
            object: false,
            entry: Box::new(|n| format!(r#"[["breaker",{n}],0]"#)),
            len: |state| Some(state.table_sizes().starved_episodes),
        },
        DecodeCap {
            pointer: "/grouper/cgroups/0/1/entries",
            limit: MAX_ENTRIES_PER_CGROUP,
            object: false,
            entry: Box::new(move |_| group_entry.clone()),
            len: |state| {
                state
                    .grouper
                    .entries_mut_for_test(7)
                    .map(|entries| entries.len())
            },
        },
        DecodeCap {
            pointer: "/queue/entries/0/item/members",
            limit: MAX_JOB_MEMBERS,
            object: false,
            entry: Box::new({
                let member = member.clone();
                move |_| member.clone()
            }),
            len: |state| {
                state
                    .queue
                    .entries
                    .first()
                    .map(|entry| entry.item.members.len())
            },
        },
        DecodeCap {
            pointer: "/in_flight/0/job/members",
            limit: MAX_JOB_MEMBERS,
            object: false,
            entry: Box::new(move |_| member.clone()),
            len: |state| {
                state
                    .in_flight
                    .first()
                    .map(|flight| flight.job.members.len())
            },
        },
    ];
    // Der Eintrag über der Grenze ist selbst kein JSON (bei einem Objekt
    // endet schon sein Schlüssel nie): würde er auch nur übersprungen oder
    // der Rest geparst, käme „json encoding".
    let overflow = |case: &DecodeCap| {
        if case.object {
            r#","overflow!!!"#
        } else {
            r#",!!!"#
        }
    };
    for case in &cases {
        // Genau der Höchstwert wird vollständig dekodiert und besteht die
        // Vorabprüfung von `restore` (beide Stufen haben dieselbe Grenze).
        let mut state =
            DispatchState::from_json_bounded(&with_table(&base, case, case.limit, "")?)?;
        assert_eq!((case.len)(&mut state), Some(case.limit), "{}", case.pointer);
        assert_eq!(
            state.table_sizes().check_within(&TableSizes::LEGAL_MAX),
            Ok(()),
            "{}",
            case.pointer
        );
        assert_eq!(state.check_slot_sizes(), Ok(()), "{}", case.pointer);
        // Ein Eintrag mehr bricht das Dekodieren ab.
        let beyond = with_table(&base, case, case.limit, overflow(case))?;
        assert_eq!(
            DispatchState::from_json_bounded(&beyond).err(),
            invalid("too many table entries"),
            "{}",
            case.pointer
        );
    }
    // Der Fall aus dem Review wörtlich: `BUDGET_MAX_PARTITIONS_LIMIT + 1`
    // gültige Paare `[n,0]` (gut 300 KB).
    let privileges = cases
        .iter()
        .find(|case| case.pointer == "/budget/uid_privileged")
        .ok_or_else(missing("uid privileges"))?;
    let one_more = with_table(
        &base,
        privileges,
        BUDGET_MAX_PARTITIONS_LIMIT.saturating_add(1),
        "",
    )?;
    assert!(one_more.len() < 512 << 10);
    assert_eq!(
        DispatchState::from_json_bounded(&one_more).err(),
        invalid("too many table entries")
    );
    Ok(())
}

#[test]
fn test_the_entry_limit_label_cannot_be_forged_from_the_input() -> TestResult {
    let json = rich_state()?;
    // Die feste Meldung als Ursache und als Zahl: die Meldungen von serde
    // beginnen mit eigenen Wörtern („unknown variant", „invalid type"), nie
    // mit Text aus der Eingabe.
    let mut variant = json.clone();
    set(
        &mut variant,
        "/starved_episodes",
        json!([[[TOO_MANY_ENTRIES, 1], NOW]]),
    )?;
    assert_eq!(decode_result(&variant)?.err(), invalid("json encoding"));
    let mut number = json.clone();
    set(&mut number, "/next_job", Value::from(TOO_MANY_ENTRIES))?;
    assert_eq!(decode_result(&number)?.err(), invalid("json encoding"));
    // Als Feldname: unbekannte Felder ignoriert serde (kein
    // `deny_unknown_fields`), es entsteht also gar keine Meldung, und der
    // Zustand wird gelesen.
    let mut field = json;
    field
        .as_object_mut()
        .ok_or_else(missing("state object"))?
        .insert(TOO_MANY_ENTRIES.to_owned(), Value::from(TOO_MANY_ENTRIES));
    assert!(decode_result(&field)?.is_ok());
    Ok(())
}

#[test]
fn test_decode_failure_compares_the_message_without_copying_it() -> TestResult {
    // Fehler mit genau gewählter Meldung (ein Datenfehler wie die der
    // Visitors).
    let error = |message: &str| <serde_json::Error as serde::de::Error>::custom(message);
    // Die Marke allein und mit Anhang (so kommt sie aus `serde_json`).
    assert_eq!(
        decode_failure(&error(TOO_MANY_ENTRIES)),
        "too many table entries"
    );
    assert_eq!(
        decode_failure(&error(
            format!("{TOO_MANY_ENTRIES} at line 1 column 9").as_str()
        )),
        "too many table entries"
    );
    // Ein Anfang der Marke, eine Abweichung im letzten Byte und die Marke
    // nach anderem Text zählen nicht.
    let (head, _) = TOO_MANY_ENTRIES
        .split_at_checked(TOO_MANY_ENTRIES.len().saturating_sub(1))
        .ok_or_else(missing("label head"))?;
    for other in [
        head.to_owned(),
        format!("{head}!"),
        format!("x{TOO_MANY_ENTRIES}"),
        String::new(),
    ] {
        assert_eq!(
            decode_failure(&error(other.as_str())),
            "json encoding",
            "{other}"
        );
    }
    // Eine lange Meldung wird nach dem ersten abweichenden Byte nicht
    // weiter gelesen und nie kopiert; das Ergebnis ist dasselbe.
    let long = format!("invalid type: string \"{}\"", "s".repeat(1 << 20));
    assert_eq!(decode_failure(&error(long.as_str())), "json encoding");
    // Ein Syntaxfehler ist nie die Eintragsgrenze.
    let syntax = serde_json::from_slice::<Value>(b"[").err();
    assert_eq!(syntax.as_ref().map(decode_failure), Some("json encoding"));
    Ok(())
}

#[test]
fn test_decoding_accepts_only_bytes_to_json_writes() -> TestResult {
    // Erlaubt sind genau druckbares ASCII ohne Backslash und JSON-Leerraum.
    let allowed: Vec<u8> = (0..=u8::MAX)
        .filter(|byte| check_state_encoding(&[*byte]).is_ok())
        .collect();
    let mut expected: Vec<u8> = (b' '..=b'~')
        .filter(|byte| *byte != b'\\')
        .chain([b'\t', b'\n', b'\r'])
        .collect();
    expected.sort_unstable();
    assert_eq!(allowed, expected);
    assert_eq!(check_state_encoding(b""), Ok(()));
    // Der Fall aus dem Review, verkleinert: rohe `0x7F` in einem
    // Skalarfeld. serde gäbe sie in der Meldung als `\u{7f}` wieder (sechs
    // Bytes je Byte der Eingabe); die Prüfung lehnt sie ab, bevor serde
    // läuft. Ebenso C1-Steuerzeichen (`U+0085`, dreifach) und Nicht-ASCII.
    for raw in [
        b"\x7f".as_slice(),
        "\u{85}".as_bytes(),
        "\u{202e}".as_bytes(),
    ] {
        let mut hostile = b"{\"version\":\"".to_vec();
        for _ in 0..1_024 {
            hostile.extend_from_slice(raw);
        }
        hostile.extend_from_slice(b"\"}");
        assert_eq!(
            check_state_encoding(&hostile).err(),
            invalid("json encoding")
        );
        assert_eq!(
            DispatchState::from_json_bounded(&hostile).err(),
            invalid("json encoding")
        );
    }
    let state = decode(&rich_state()?)?;
    let text = serde_json::to_string(&state)?;
    // `to_json` hält sich daran, und ein Escape, das serde annähme (hier
    // `-` als `\u002d` im Sensor), lehnt schon die Prüfung ab: der Inhalt
    // wäre gültig, die Kodierung ist es nicht.
    assert_eq!(check_state_encoding(text.as_bytes()), Ok(()));
    assert!(text.contains("\"probe-bpf-0\""));
    let escaped = text.replacen("\"probe-bpf-0\"", "\"probe\\u002dbpf-0\"", 1);
    assert_eq!(
        serde_json::from_str::<StateRepr>(&escaped)
            .map(StateRepr::into_state)
            .ok(),
        Some(state.clone())
    );
    assert_eq!(
        DispatchState::from_json_bounded(escaped.as_bytes()).err(),
        invalid("json encoding")
    );
    // Die Prüfung läuft vor serde: hinter einer Tabelle über ihrem
    // Höchstwert, an die serde nie käme, entscheidet ein rohes Byte.
    let mut over = rich_state()?;
    let episodes: Vec<Value> = (0..=MAX_STARVED_EPISODES)
        .map(|n| json!([["breaker", n], 0]))
        .collect();
    set(&mut over, "/starved_episodes", Value::from(episodes))?;
    let mut bytes = serde_json::to_vec(&over)?;
    assert_eq!(
        DispatchState::from_json_bounded(&bytes).err(),
        invalid("too many table entries")
    );
    bytes.push(0x7F);
    assert_eq!(
        DispatchState::from_json_bounded(&bytes).err(),
        invalid("json encoding")
    );
    // JSON-Leerraum bleibt lesbar: eingerückt und mit Zeilenende.
    let pretty = serde_json::to_vec_pretty(&state)?;
    assert!(pretty.contains(&b'\n'));
    assert_eq!(DispatchState::from_json_bounded(&pretty)?, state);
    let mut line = state.to_json()?;
    line.extend_from_slice(b"\r\n");
    assert_eq!(DispatchState::from_json_bounded(&line)?, state);
    // `to_json` schreibt nie, was `from_json_bounded` ablehnt: ein in der
    // Crate gebauter Zustand mit Nicht-ASCII (über `Dispatcher` nicht
    // erreichbar) wird nicht kodiert.
    let mut foreign = state;
    foreign
        .pending_asks
        .values_mut()
        .next()
        .ok_or_else(missing("pending ask"))?
        .finding
        .sensor = "probe\u{202e}".to_owned();
    assert_eq!(foreign.to_json().err(), invalid("json encoding"));
    Ok(())
}

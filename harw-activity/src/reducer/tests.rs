//! Unit tests for [`Reducer`] matching, sealing, rebase and ordering rules.

use super::*;
use crate::event::{ActivityOwner, parse_jsonl, to_jsonl};
use crate::pattern::Phase;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const FS_READ: ActivityDescriptor =
    ActivityDescriptor::new("filesystem", "read", Some("path"), Effect::Observe);
const FS_EDIT: ActivityDescriptor =
    ActivityDescriptor::new("filesystem", "edit", Some("path"), Effect::Mutate);
const FS_CREATE: ActivityDescriptor =
    ActivityDescriptor::new("filesystem", "create", Some("path"), Effect::Create);
const FS_DELETE: ActivityDescriptor =
    ActivityDescriptor::new("filesystem", "delete", Some("path"), Effect::Delete);
const SHELL: ActivityDescriptor = ActivityDescriptor::new("process", "exec", None, Effect::Execute);
const JOB_START: ActivityDescriptor =
    ActivityDescriptor::new("job", "start", None, Effect::Execute);
const JOB_STATUS: ActivityDescriptor =
    ActivityDescriptor::new("job", "status", Some("work_id"), Effect::Observe);
const OTHER_START: ActivityDescriptor =
    ActivityDescriptor::new("deploy", "start", None, Effect::Execute);
const SCRIPT: ActivityDescriptor =
    ActivityDescriptor::new("process", "run", Some("script"), Effect::Execute);
const FS_MOVE: ActivityDescriptor =
    ActivityDescriptor::new("filesystem", "move", Some("path"), Effect::Mutate);
const MESSAGE: ActivityDescriptor =
    ActivityDescriptor::new("messaging", "send", Some("to"), Effect::Communicate);

fn path(p: &str) -> ActivityKey {
    ActivityKey::Path(p.to_owned())
}

fn job(w: &str) -> ActivityKey {
    ActivityKey::Job(w.to_owned())
}

fn call_key(c: &str) -> ActivityKey {
    ActivityKey::Call(c.to_owned())
}

/// Tool call event of session `root`, turn `turn`.
fn call(
    seq: u64,
    turn: u64,
    id: &str,
    descriptor: ActivityDescriptor,
    resource: Option<ActivityKey>,
    lifecycle: Lifecycle,
) -> ActivityEvent {
    let mut event = ActivityEvent::new(
        seq,
        ActivityOwner::session("root").with_turn(turn).with_call(id),
        descriptor,
        lifecycle,
    );
    event.resource = resource;
    event
}

fn read(seq: u64, id: &str, p: &str, lifecycle: Lifecycle) -> ActivityEvent {
    call(seq, 1, id, FS_READ, Some(path(p)), lifecycle)
}

fn done(seq: u64, id: &str, descriptor: ActivityDescriptor, p: &str) -> ActivityEvent {
    call(seq, 1, id, descriptor, Some(path(p)), Lifecycle::Finished)
        .with_outcome(Outcome::Succeeded)
}

fn boundary(seq: u64, session: &str, turn: Option<u64>, kind: Boundary) -> ActivityEvent {
    let mut owner = ActivityOwner::session(session);
    owner.turn = turn;
    ActivityEvent::new(
        seq,
        owner,
        ActivityDescriptor::SCOPE,
        Lifecycle::Boundary(kind),
    )
}

fn rebase(seq: u64, session: &str, from: ActivityKey, to: ActivityKey) -> ActivityEvent {
    ActivityEvent::new(
        seq,
        ActivityOwner::session(session).with_turn(1),
        JOB_START,
        Lifecycle::Rebase { to },
    )
    .with_resource(from)
}

fn job_run(seq: u64, w: &str, lifecycle: Lifecycle, outcome: Option<Outcome>) -> ActivityEvent {
    let mut event = ActivityEvent::new(
        seq,
        ActivityOwner::session("root"),
        ActivityDescriptor::JOB_RUN,
        lifecycle,
    )
    .with_resource(job(w));
    event.outcome = outcome;
    event
}

fn agent_run(
    seq: u64,
    agent: &str,
    role: Option<&str>,
    lifecycle: Lifecycle,
    outcome: Option<Outcome>,
) -> ActivityEvent {
    let mut owner = ActivityOwner::session(agent).with_parent("root");
    owner.role = role.map(str::to_owned);
    let mut event = ActivityEvent::new(seq, owner, ActivityDescriptor::AGENT_RUN, lifecycle)
        .with_resource(ActivityKey::Agent(agent.to_owned()));
    event.outcome = outcome;
    event
}

fn id(opened_at: u64, pattern: PatternId) -> ProjectionId {
    ProjectionId { opened_at, pattern }
}

fn instance(
    reducer: &Reducer,
    opened_at: u64,
    pattern: PatternId,
) -> Result<&PatternInstance, String> {
    reducer
        .instance(&id(opened_at, pattern))
        .ok_or_else(|| format!("no instance {pattern}#{opened_at}"))
}

fn verbs(instance: &PatternInstance) -> Vec<(String, u64)> {
    instance
        .state
        .verbs
        .iter()
        .map(|tally| (tally.verb.to_string(), tally.counts.total))
        .collect()
}

fn root_call(id: &str) -> ConstituentId {
    ConstituentId::Call {
        session: "root".into(),
        call: id.into(),
    }
}

#[test]
fn same_path_reads_share_one_burst_and_turn_end_seals_it() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&read(1, "c1", "a", Lifecycle::Started));
    reducer.apply(&done(2, "c1", FS_READ, "a"));
    let updates = reducer.apply(&done(3, "c2", FS_READ, "a"));
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].id, id(1, PatternId::FILE_READ_BURST));
    assert_eq!(updates[0].revision, 3);

    let burst = instance(&reducer, 1, PatternId::FILE_READ_BURST)?;
    assert_eq!(burst.key, Some(path("a")));
    assert_eq!(burst.state.counts.total, 2);
    assert_eq!(burst.state.phase, Phase::Quiescent);
    assert_eq!(burst.constituents, vec![1, 2, 3]);

    let sealed = reducer.apply(&boundary(4, "root", Some(1), Boundary::TurnEnd));
    assert_eq!(sealed.len(), 1);
    assert!(sealed[0].instance.is_terminal());
    assert_eq!(sealed[0].revision, 4);
    // The boundary is not a constituent of the burst.
    assert_eq!(sealed[0].instance.constituents, vec![1, 2, 3]);

    // Turn 2 opens a fresh burst for the same path.
    reducer.apply(&call(
        5,
        2,
        "c3",
        FS_READ,
        Some(path("a")),
        Lifecycle::Started,
    ));
    assert!(
        reducer
            .instance(&id(5, PatternId::FILE_READ_BURST))
            .is_some()
    );
    Ok(())
}

#[test]
fn different_paths_never_merge() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&done(1, "c1", FS_READ, "a"));
    reducer.apply(&done(2, "c2", FS_READ, "b"));
    reducer.apply(&done(3, "c3", FS_READ, "a"));
    assert_eq!(
        instance(&reducer, 1, PatternId::FILE_READ_BURST)?
            .state
            .counts
            .total,
        2
    );
    assert_eq!(
        instance(&reducer, 2, PatternId::FILE_READ_BURST)?
            .state
            .counts
            .total,
        1
    );
    assert_eq!(reducer.instances().count(), 2);
    Ok(())
}

#[test]
fn overlapping_reads_of_one_path_are_concurrent_constituents() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&read(1, "c1", "a", Lifecycle::Started));
    let both = reducer.apply(&read(2, "c2", "a", Lifecycle::Started));
    assert_eq!(both[0].instance.state.counts.running, 2);
    assert_eq!(both[0].instance.state.phase, Phase::Open);

    let one = reducer.apply(&done(3, "c1", FS_READ, "a"));
    assert_eq!(one[0].instance.state.counts.running, 1);
    assert_eq!(one[0].instance.state.phase, Phase::Open);

    let none = reducer.apply(&done(4, "c2", FS_READ, "a"));
    let state = &none[0].instance.state;
    assert_eq!((state.counts.total, state.counts.running), (2, 0));
    assert_eq!(state.phase, Phase::Quiescent);
    assert_eq!(none[0].id, both[0].id);
    Ok(())
}

#[test]
fn edit_then_reads_form_one_loop_in_first_seen_verb_order() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&done(1, "c1", FS_EDIT, "a"));
    for (seq, call_id) in [(2, "c2"), (3, "c3"), (4, "c4")] {
        reducer.apply(&done(seq, call_id, FS_READ, "a"));
    }
    let lp = instance(&reducer, 1, PatternId::FILE_EDIT_READ_LOOP)?;
    assert_eq!(
        verbs(lp),
        vec![("edit".to_owned(), 1), ("read".to_owned(), 3)]
    );
    assert_eq!(reducer.instances().count(), 1);

    // Further edits of the same file keep updating the same loop.
    reducer.apply(&done(5, "c5", FS_CREATE, "a"));
    let lp = instance(&reducer, 1, PatternId::FILE_EDIT_READ_LOOP)?;
    assert_eq!(lp.state.counts.total, 5);
    Ok(())
}

#[test]
fn edit_after_reads_seals_the_burst_and_opens_a_loop() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&done(1, "c1", FS_READ, "a"));
    let updates = reducer.apply(&done(2, "c2", FS_EDIT, "a"));
    let ids: Vec<ProjectionId> = updates.iter().map(|update| update.id).collect();
    assert_eq!(
        ids,
        vec![
            id(1, PatternId::FILE_READ_BURST),
            id(2, PatternId::FILE_EDIT_READ_LOOP)
        ]
    );
    assert!(updates[0].instance.is_terminal());
    reducer.apply(&done(3, "c3", FS_READ, "a"));
    assert_eq!(
        verbs(instance(&reducer, 2, PatternId::FILE_EDIT_READ_LOOP)?),
        vec![("edit".to_owned(), 1), ("read".to_owned(), 1)]
    );
    Ok(())
}

#[test]
fn failed_mutating_constituent_stays_visible_in_compact_counts() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(
        &call(1, 1, "c1", FS_EDIT, Some(path("a")), Lifecycle::Finished)
            .with_outcome(Outcome::failed("conflict")),
    );
    reducer.apply(&done(2, "c2", FS_READ, "a"));
    let lp = instance(&reducer, 1, PatternId::FILE_EDIT_READ_LOOP)?;
    assert_eq!(lp.failed(), 1);
    assert_eq!(lp.state.verbs[0].counts.failed, 1);
    let failed: u64 = lp.state.members.iter().map(Constituent::failed).sum();
    assert_eq!(lp.state.counts.failed, failed);
    Ok(())
}

#[test]
fn failure_is_not_hidden_by_a_later_success_report() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(
        &call(1, 1, "c1", FS_READ, Some(path("a")), Lifecycle::Finished)
            .with_outcome(Outcome::failed("io")),
    );
    reducer.apply(&done(2, "c1", FS_READ, "a"));
    let burst = instance(&reducer, 1, PatternId::FILE_READ_BURST)?;
    assert_eq!(burst.failed(), 1);
    assert_eq!(burst.state.counts.succeeded, 0);
    Ok(())
}

#[test]
fn unknown_descriptor_is_standalone_and_breaks_the_open_burst() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&done(1, "c1", FS_READ, "a"));
    let updates = reducer.apply(&call(
        2,
        1,
        "c2",
        ActivityDescriptor::UNKNOWN,
        Some(path("a")),
        Lifecycle::Started,
    ));
    assert_eq!(updates.len(), 2);
    assert!(updates[0].instance.is_terminal());
    let standalone = &updates[1].instance;
    assert_eq!(standalone.pattern, PatternId::STANDALONE);
    assert_eq!(standalone.state.phase, Phase::Open);
    assert_eq!(standalone.key, Some(path("a")));

    let finished = reducer.apply(
        &call(
            3,
            1,
            "c2",
            ActivityDescriptor::UNKNOWN,
            None,
            Lifecycle::Finished,
        )
        .with_outcome(Outcome::failed("boom")),
    );
    assert_eq!(finished.len(), 1);
    assert!(finished[0].instance.is_terminal());
    assert_eq!(finished[0].instance.failed(), 1);

    reducer.apply(&done(4, "c3", FS_READ, "a"));
    assert!(
        reducer
            .instance(&id(4, PatternId::FILE_READ_BURST))
            .is_some()
    );
    Ok(())
}

#[test]
fn observe_without_path_and_other_keys_are_standalone() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&call(1, 1, "c1", FS_READ, None, Lifecycle::Finished));
    reducer.apply(&call(
        2,
        1,
        "c2",
        FS_READ,
        Some(ActivityKey::Other("docs.rs".into())),
        Lifecycle::Finished,
    ));
    reducer.apply(&call(
        3,
        1,
        "c3",
        OTHER_START,
        Some(call_key("c3")),
        Lifecycle::Started,
    ));
    let patterns: Vec<PatternId> = reducer.instances().map(|i| i.pattern).collect();
    assert_eq!(patterns, vec![PatternId::STANDALONE; 3]);
    Ok(())
}

#[test]
fn delete_of_a_path_seals_its_instances_and_stays_standalone() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&done(1, "c1", FS_READ, "a"));
    reducer.apply(&done(2, "c2", FS_READ, "b"));
    let updates = reducer.apply(&done(3, "c3", FS_DELETE, "a"));
    let ids: Vec<ProjectionId> = updates.iter().map(|update| update.id).collect();
    assert_eq!(
        ids,
        vec![
            id(1, PatternId::FILE_READ_BURST),
            id(3, PatternId::STANDALONE)
        ]
    );
    assert!(
        !instance(&reducer, 2, PatternId::FILE_READ_BURST)?
            .state
            .sealed
    );
    Ok(())
}

#[test]
fn unkeyed_execute_seals_the_turn_but_a_keyed_write_elsewhere_does_not() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&done(1, "c1", FS_READ, "a"));
    reducer.apply(&done(2, "c2", FS_EDIT, "b"));
    assert!(
        !instance(&reducer, 1, PatternId::FILE_READ_BURST)?
            .state
            .sealed
    );

    // Another session's shell call does not touch root's instances.
    let mut foreign = call(3, 1, "x1", SHELL, None, Lifecycle::Started);
    foreign.owner.session = "other".into();
    assert_eq!(reducer.apply(&foreign).len(), 1);

    let updates = reducer.apply(&call(4, 1, "c3", SHELL, None, Lifecycle::Started));
    assert_eq!(updates.len(), 3);
    assert!(updates[0].instance.state.sealed);
    assert!(updates[1].instance.state.sealed);
    assert_eq!(updates[2].instance.pattern, PatternId::STANDALONE);
    Ok(())
}

#[test]
fn a_path_keyed_execute_seals_the_bursts_of_other_paths() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&done(1, "c1", FS_READ, "a"));
    let updates = reducer.apply(&done(2, "c2", SCRIPT, "scripts/x.sh"));
    let ids: Vec<ProjectionId> = updates.iter().map(|update| update.id).collect();
    assert_eq!(
        ids,
        vec![
            id(1, PatternId::FILE_READ_BURST),
            id(2, PatternId::STANDALONE)
        ]
    );
    assert!(updates[0].instance.is_terminal());
    reducer.apply(&done(3, "c3", FS_READ, "a"));
    for opened_at in [1, 3] {
        assert_eq!(
            instance(&reducer, opened_at, PatternId::FILE_READ_BURST)?
                .state
                .counts
                .total,
            1
        );
    }
    Ok(())
}

#[test]
fn turn_end_only_seals_its_own_turn_while_timeout_seals_the_session() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&call(
        1,
        1,
        "c1",
        FS_READ,
        Some(path("a")),
        Lifecycle::Finished,
    ));
    reducer.apply(&call(
        2,
        2,
        "c2",
        FS_READ,
        Some(path("a")),
        Lifecycle::Finished,
    ));
    let turn_one = reducer.apply(&boundary(3, "root", Some(1), Boundary::TurnEnd));
    assert_eq!(turn_one.len(), 1);
    assert_eq!(turn_one[0].id, id(1, PatternId::FILE_READ_BURST));
    assert!(
        reducer
            .apply(&boundary(4, "other", None, Boundary::Timeout))
            .is_empty()
    );
    let timeout = reducer.apply(&boundary(5, "root", None, Boundary::Timeout));
    assert_eq!(timeout.len(), 1);
    assert_eq!(timeout[0].id, id(2, PatternId::FILE_READ_BURST));
    assert!(
        reducer
            .apply(&boundary(6, "root", None, Boundary::SessionEnd))
            .is_empty()
    );
    Ok(())
}

#[test]
fn job_rebase_keeps_the_projection_id_through_the_whole_lifecycle() -> TestResult {
    let mut reducer = Reducer::new();
    let mut seen = BTreeSet::new();
    let mut apply = |reducer: &mut Reducer, event: ActivityEvent| {
        for update in reducer.apply(&event) {
            seen.insert(update.id);
        }
    };
    apply(
        &mut reducer,
        call(
            1,
            1,
            "c1",
            JOB_START,
            Some(call_key("c1")),
            Lifecycle::Started,
        ),
    );
    apply(&mut reducer, rebase(2, "root", call_key("c1"), job("W1")));
    apply(
        &mut reducer,
        call(
            3,
            1,
            "c1",
            JOB_START,
            Some(call_key("c1")),
            Lifecycle::Finished,
        ),
    );
    apply(
        &mut reducer,
        call(4, 1, "c2", JOB_STATUS, Some(job("W1")), Lifecycle::Started),
    );
    apply(
        &mut reducer,
        call(5, 1, "c2", JOB_STATUS, Some(job("W1")), Lifecycle::Finished),
    );
    apply(&mut reducer, job_run(6, "W1", Lifecycle::Progress, None));
    apply(
        &mut reducer,
        job_run(
            7,
            "W1",
            Lifecycle::Finished,
            Some(Outcome::failed("exit-2")),
        ),
    );

    let only = id(1, PatternId::JOB_LIFECYCLE);
    assert_eq!(seen.into_iter().collect::<Vec<_>>(), vec![only]);
    let lifecycle = instance(&reducer, 1, PatternId::JOB_LIFECYCLE)?;
    assert_eq!(lifecycle.key, Some(job("W1")));
    assert_eq!(lifecycle.state.aliases, vec![call_key("c1")]);
    assert!(lifecycle.is_terminal());
    assert_eq!(lifecycle.failed(), 1);
    assert_eq!(
        verbs(lifecycle),
        vec![
            ("start".to_owned(), 1),
            ("run".to_owned(), 1),
            ("status".to_owned(), 1)
        ]
    );
    assert_eq!(reducer.job_projection("W1"), Some(only));
    assert_eq!(reducer.projection_of(&root_call("c1")), Some(only));
    assert_eq!(
        reducer.projection_of(&ConstituentId::Key(job("W1"))),
        Some(only)
    );
    Ok(())
}

#[test]
fn rebase_onto_a_bound_work_id_is_refused_visibly() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&call(
        1,
        1,
        "c1",
        JOB_START,
        Some(call_key("c1")),
        Lifecycle::Started,
    ));
    reducer.apply(&call(
        2,
        1,
        "c2",
        JOB_START,
        Some(call_key("c2")),
        Lifecycle::Started,
    ));
    reducer.apply(&rebase(3, "root", call_key("c1"), job("W1")));
    let refused = reducer.apply(&rebase(4, "root", call_key("c2"), job("W1")));
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].id, id(4, PatternId::STANDALONE));
    assert_eq!(
        refused[0].instance.state.anomaly,
        Some(Anomaly::RebaseConflict)
    );

    let provisional = instance(&reducer, 2, PatternId::JOB_LIFECYCLE)?;
    assert_eq!(provisional.key, Some(call_key("c2")));
    assert!(provisional.state.aliases.is_empty());
    assert_eq!(
        reducer.job_projection("W1"),
        Some(id(1, PatternId::JOB_LIFECYCLE))
    );

    // A repeated identical binding is idempotent provenance, not a conflict.
    let again = reducer.apply(&rebase(5, "root", call_key("c1"), job("W1")));
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].id, id(1, PatternId::JOB_LIFECYCLE));
    assert_eq!(again[0].instance.state.members.len(), 2);
    Ok(())
}

#[test]
fn rebase_without_a_bound_key_is_an_unmatched_standalone() -> TestResult {
    let mut reducer = Reducer::new();
    let updates = reducer.apply(&rebase(1, "root", call_key("nope"), job("W1")));
    assert_eq!(updates.len(), 1);
    let refused = &updates[0].instance;
    assert_eq!(refused.state.anomaly, Some(Anomaly::RebaseUnmatched));
    assert!(refused.is_terminal());
    assert_eq!(reducer.job_projection("W1"), None);

    // Mismatched key kinds are refused too, `Job` -> `Job` included.
    let updates = reducer.apply(&rebase(2, "root", path("a"), job("W2")));
    assert_eq!(
        updates[0].instance.state.anomaly,
        Some(Anomaly::RebaseUnmatched)
    );
    reducer.apply(&job_run(3, "W1", Lifecycle::Started, None));
    let updates = reducer.apply(&rebase(4, "root", job("W1"), job("W2")));
    assert_eq!(updates.len(), 1);
    assert_eq!(
        updates[0].instance.state.anomaly,
        Some(Anomaly::RebaseUnmatched)
    );
    assert_eq!(reducer.job_projection("W2"), None);
    assert_eq!(
        instance(&reducer, 3, PatternId::JOB_LIFECYCLE)?
            .state
            .members
            .len(),
        1
    );
    Ok(())
}

#[test]
fn a_provisional_call_binds_at_most_one_work_id() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&call(
        1,
        1,
        "c1",
        JOB_START,
        Some(call_key("c1")),
        Lifecycle::Started,
    ));
    reducer.apply(&rebase(2, "root", call_key("c1"), job("W1")));
    let refused = reducer.apply(&rebase(3, "root", call_key("c1"), job("W2")));
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0].id, id(3, PatternId::STANDALONE));
    assert_eq!(
        refused[0].instance.state.anomaly,
        Some(Anomaly::RebaseConflict)
    );

    let only = id(1, PatternId::JOB_LIFECYCLE);
    let lifecycle = instance(&reducer, 1, PatternId::JOB_LIFECYCLE)?;
    assert_eq!(lifecycle.key, Some(job("W1")));
    assert_eq!(lifecycle.state.aliases, vec![call_key("c1")]);
    assert_eq!(lifecycle.state.counts.total, 2);
    assert_eq!(lifecycle.state.counts.running, 2);
    assert_eq!(reducer.job_projection("W1"), Some(only));
    assert_eq!(reducer.job_projection("W2"), None);

    // W2 stays an unrelated job of its own.
    reducer.apply(&job_run(4, "W2", Lifecycle::Started, None));
    assert_eq!(
        reducer.job_projection("W2"),
        Some(id(4, PatternId::JOB_LIFECYCLE))
    );
    assert_eq!(
        instance(&reducer, 1, PatternId::JOB_LIFECYCLE)?
            .state
            .counts
            .total,
        2
    );
    Ok(())
}

#[test]
fn a_runtime_event_before_the_rebase_yields_two_visible_jobs() -> TestResult {
    // Adapter contract rule 2: the rebase must come first. If it does not,
    // nothing is merged and the conflict is visible.
    let mut reducer = Reducer::new();
    reducer.apply(&call(
        1,
        1,
        "c1",
        JOB_START,
        Some(call_key("c1")),
        Lifecycle::Started,
    ));
    reducer.apply(&job_run(2, "W1", Lifecycle::Started, None));
    let refused = reducer.apply(&rebase(3, "root", call_key("c1"), job("W1")));
    assert_eq!(
        refused[0].instance.state.anomaly,
        Some(Anomaly::RebaseConflict)
    );
    let jobs = reducer
        .instances()
        .filter(|instance| instance.pattern == PatternId::JOB_LIFECYCLE)
        .count();
    assert_eq!(jobs, 2);
    Ok(())
}

#[test]
fn same_call_id_in_two_sessions_never_shares_a_job() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&call(
        1,
        1,
        "c1",
        JOB_START,
        Some(call_key("c1")),
        Lifecycle::Started,
    ));
    let mut child = call(
        2,
        1,
        "c1",
        JOB_START,
        Some(call_key("c1")),
        Lifecycle::Started,
    );
    child.owner.session = "child".into();
    reducer.apply(&child);
    assert_eq!(reducer.instances().count(), 2);

    reducer.apply(&rebase(3, "child", call_key("c1"), job("W9")));
    assert_eq!(
        reducer.job_projection("W9"),
        Some(id(2, PatternId::JOB_LIFECYCLE))
    );
    assert_eq!(
        instance(&reducer, 1, PatternId::JOB_LIFECYCLE)?.key,
        Some(call_key("c1"))
    );
    Ok(())
}

#[test]
fn failed_start_seals_the_provisional_job_and_later_calls_are_standalone() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&call(
        1,
        1,
        "c1",
        JOB_START,
        Some(call_key("c1")),
        Lifecycle::Started,
    ));
    let updates = reducer.apply(
        &call(
            2,
            1,
            "c1",
            JOB_START,
            Some(call_key("c1")),
            Lifecycle::Finished,
        )
        .with_outcome(Outcome::failed("spawn")),
    );
    assert!(updates[0].instance.is_terminal());
    assert_eq!(updates[0].instance.failed(), 1);

    let mut reducer = Reducer::new();
    reducer.apply(&job_run(1, "W1", Lifecycle::Started, None));
    reducer.apply(&job_run(
        2,
        "W1",
        Lifecycle::Finished,
        Some(Outcome::Succeeded),
    ));
    assert!(instance(&reducer, 1, PatternId::JOB_LIFECYCLE)?.is_terminal());
    let late = reducer.apply(&call(
        3,
        1,
        "c9",
        JOB_STATUS,
        Some(job("W1")),
        Lifecycle::Finished,
    ));
    assert_eq!(late[0].id, id(3, PatternId::STANDALONE));
    Ok(())
}

#[test]
fn agent_failures_aggregate_only_by_parent_role_and_class() -> TestResult {
    let mut reducer = Reducer::new();
    let failed = |class: &str| Some(Outcome::failed(class));
    reducer.apply(&agent_run(
        1,
        "a1",
        Some("reviewer"),
        Lifecycle::Started,
        None,
    ));
    reducer.apply(&agent_run(
        2,
        "a2",
        Some("reviewer"),
        Lifecycle::Started,
        None,
    ));
    let first = reducer.apply(&agent_run(
        3,
        "a1",
        Some("reviewer"),
        Lifecycle::Finished,
        failed("timeout"),
    ));
    let ids: Vec<ProjectionId> = first.iter().map(|update| update.id).collect();
    assert_eq!(
        ids,
        vec![
            id(1, PatternId::STANDALONE),
            id(3, PatternId::AGENT_ROLE_FAILURE_BURST)
        ]
    );
    reducer.apply(&agent_run(
        4,
        "a2",
        Some("reviewer"),
        Lifecycle::Finished,
        failed("timeout"),
    ));
    reducer.apply(&agent_run(
        5,
        "a3",
        Some("reviewer"),
        Lifecycle::Finished,
        failed("crash"),
    ));
    reducer.apply(&agent_run(
        6,
        "a4",
        None,
        Lifecycle::Finished,
        failed("timeout"),
    ));
    reducer.apply(&agent_run(
        7,
        "a5",
        Some("reviewer"),
        Lifecycle::Started,
        None,
    ));

    let burst = instance(&reducer, 3, PatternId::AGENT_ROLE_FAILURE_BURST)?;
    assert_eq!(burst.state.counts.total, 2);
    assert_eq!(burst.state.counts.failed, 2);
    assert_eq!(
        burst.state.scope,
        Scope::Parent {
            parent: Some("root".into()),
            role: Some("reviewer".into()),
            failure_class: Some("timeout".into()),
        }
    );
    let crash = instance(&reducer, 5, PatternId::AGENT_ROLE_FAILURE_BURST)?;
    assert_eq!(crash.state.counts.total, 1);
    // No role: never aggregated, sealed at once.
    let roleless = instance(&reducer, 6, PatternId::AGENT_ROLE_FAILURE_BURST)?;
    assert!(roleless.is_terminal());
    // The running child stays an individual, open instance and joins no burst.
    let running = instance(&reducer, 7, PatternId::STANDALONE)?;
    assert_eq!(running.state.phase, Phase::Open);
    let a5 = ConstituentId::Key(ActivityKey::Agent("a5".into()));
    assert_eq!(reducer.failure_burst_of(&a5), None);
    // Each failed run keeps its concrete identity in its own instance too.
    let a1 = ConstituentId::Key(ActivityKey::Agent("a1".into()));
    assert_eq!(
        reducer.projection_of(&a1),
        Some(id(1, PatternId::STANDALONE))
    );
    assert_eq!(
        reducer.failure_burst_of(&a1),
        Some(id(3, PatternId::AGENT_ROLE_FAILURE_BURST))
    );

    // The parent's turn end closes the window.
    let sealed = reducer.apply(&boundary(8, "root", Some(1), Boundary::TurnEnd));
    assert_eq!(sealed.len(), 2);
    reducer.apply(&agent_run(
        9,
        "a6",
        Some("reviewer"),
        Lifecycle::Finished,
        failed("timeout"),
    ));
    assert!(
        reducer
            .instance(&id(9, PatternId::AGENT_ROLE_FAILURE_BURST))
            .is_some()
    );
    Ok(())
}

#[test]
fn only_known_agent_runs_join_a_failure_burst() -> TestResult {
    let mut reducer = Reducer::new();
    // A real failed agent run opens the burst.
    reducer.apply(&agent_run(
        1,
        "a1",
        Some("reviewer"),
        Lifecycle::Finished,
        Some(Outcome::failed("timeout")),
    ));
    // Unknown semantics and other domains keyed by an agent never join it.
    let mut unknown = agent_run(
        2,
        "a2",
        Some("reviewer"),
        Lifecycle::Finished,
        Some(Outcome::failed("timeout")),
    );
    unknown.descriptor = ActivityDescriptor::UNKNOWN;
    let updates = reducer.apply(&unknown);
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].instance.pattern, PatternId::STANDALONE);

    let mut message = agent_run(
        3,
        "a3",
        Some("reviewer"),
        Lifecycle::Finished,
        Some(Outcome::failed("timeout")),
    );
    message.descriptor = MESSAGE;
    assert_eq!(reducer.apply(&message).len(), 1);

    // A run introduced by an unknown descriptor stays out even when a known
    // agent event later reports its failure.
    let mut started = agent_run(4, "a4", Some("reviewer"), Lifecycle::Started, None);
    started.descriptor = ActivityDescriptor::UNKNOWN;
    reducer.apply(&started);
    let updates = reducer.apply(&agent_run(
        5,
        "a4",
        Some("reviewer"),
        Lifecycle::Finished,
        Some(Outcome::failed("timeout")),
    ));
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].instance.failed(), 1);

    let burst = instance(&reducer, 1, PatternId::AGENT_ROLE_FAILURE_BURST)?;
    assert_eq!(burst.state.counts.total, 1);
    for agent in ["a2", "a3", "a4"] {
        let run = ConstituentId::Key(ActivityKey::Agent(agent.to_owned()));
        assert_eq!(reducer.failure_burst_of(&run), None, "{agent}");
    }
    Ok(())
}

#[test]
fn a_failure_reported_on_progress_or_start_stays_visible() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&read(1, "c1", "a", Lifecycle::Started));
    let failed =
        reducer.apply(&read(2, "c1", "a", Lifecycle::Progress).with_outcome(Outcome::failed("io")));
    let counts = failed[0].instance.state.counts;
    assert_eq!((counts.failed, counts.running), (1, 0));
    // A later success report does not hide it.
    reducer.apply(&done(3, "c1", FS_READ, "a"));
    reducer.apply(&read(4, "c2", "a", Lifecycle::Started).with_outcome(Outcome::failed("io")));
    let burst = instance(&reducer, 1, PatternId::FILE_READ_BURST)?;
    assert_eq!(burst.failed(), 2);
    assert_eq!(burst.state.counts.running, 0);
    Ok(())
}

#[test]
fn cancelled_job_rebase_does_not_bind_or_add_a_run() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&call(
        1,
        1,
        "c1",
        JOB_START,
        Some(call_key("c1")),
        Lifecycle::Started,
    ));
    let updates = reducer
        .apply(&rebase(2, "root", call_key("c1"), job("W1")).with_outcome(Outcome::Cancelled));
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].id, id(2, PatternId::STANDALONE));
    assert_eq!(updates[0].instance.state.counts.cancelled, 1);
    assert_eq!(reducer.job_projection("W1"), None);
    let job = instance(&reducer, 1, PatternId::JOB_LIFECYCLE)?;
    assert_eq!(job.key, Some(call_key("c1")));
    assert_eq!(job.state.members.len(), 1);
    Ok(())
}

#[test]
fn cancelled_path_rebase_preserves_both_open_paths() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&done(1, "c1", FS_READ, "old"));
    reducer.apply(&done(2, "c2", FS_READ, "new"));
    let updates = reducer
        .apply(&rebase(3, "root", path("old"), path("new")).with_outcome(Outcome::Cancelled));
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].id, id(3, PatternId::STANDALONE));
    assert_eq!(
        instance(&reducer, 1, PatternId::FILE_READ_BURST)?.key,
        Some(path("old"))
    );
    assert!(!instance(&reducer, 2, PatternId::FILE_READ_BURST)?.is_terminal());
    let old = reducer.apply(&done(4, "c3", FS_READ, "old"));
    let new = reducer.apply(&done(5, "c4", FS_READ, "new"));
    assert_eq!(old[0].id, id(1, PatternId::FILE_READ_BURST));
    assert_eq!(new[0].id, id(2, PatternId::FILE_READ_BURST));
    Ok(())
}

#[test]
fn a_rebase_reporting_a_failure_is_not_applied_and_stays_visible() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&call(
        1,
        1,
        "c1",
        JOB_START,
        Some(call_key("c1")),
        Lifecycle::Started,
    ));
    let refused = reducer.apply(
        &rebase(2, "root", call_key("c1"), job("W1")).with_outcome(Outcome::failed("spawn")),
    );
    assert_eq!(refused.len(), 1);
    assert_eq!(
        refused[0].instance.state.anomaly,
        Some(Anomaly::RebaseFailed)
    );
    assert_eq!(refused[0].instance.failed(), 1);
    assert_eq!(reducer.job_projection("W1"), None);
    assert_eq!(
        instance(&reducer, 1, PatternId::JOB_LIFECYCLE)?.key,
        Some(call_key("c1"))
    );

    // An unmatched rebase carrying a failure shows it too.
    let unmatched =
        reducer.apply(&rebase(3, "root", path("x"), path("y")).with_outcome(Outcome::failed("io")));
    assert_eq!(unmatched[0].instance.failed(), 1);
    Ok(())
}

#[test]
fn a_start_without_call_id_is_unpaired_and_never_holds_a_burst_open() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&done(1, "c1", FS_READ, "a"));
    let mut unpaired = read(2, "c2", "a", Lifecycle::Started);
    unpaired.owner.call = None;
    let updates = reducer.apply(&unpaired);
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].id, id(2, PatternId::STANDALONE));
    assert_eq!(updates[0].instance.state.anomaly, Some(Anomaly::Unpaired));

    let burst = instance(&reducer, 1, PatternId::FILE_READ_BURST)?;
    assert_eq!(burst.state.counts.total, 1);
    assert_eq!(burst.state.phase, Phase::Quiescent);
    reducer.apply(&boundary(3, "root", None, Boundary::SessionEnd));
    assert!(instance(&reducer, 1, PatternId::FILE_READ_BURST)?.is_terminal());

    // A finished point observation without call id is fine.
    let mut point = done(4, "c3", FS_READ, "b");
    point.owner.call = None;
    let updates = reducer.apply(&point);
    assert_eq!(updates[0].instance.pattern, PatternId::FILE_READ_BURST);
    assert_eq!(updates[0].instance.state.anomaly, None);
    Ok(())
}

#[test]
fn path_rename_rebinds_the_burst_and_refuses_collisions() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&done(1, "c1", FS_READ, "old"));
    let moved = reducer.apply(&rebase(2, "root", path("old"), path("new")));
    assert_eq!(moved[0].id, id(1, PatternId::FILE_READ_BURST));
    assert_eq!(moved[0].instance.key, Some(path("new")));
    assert_eq!(moved[0].instance.state.aliases, vec![path("old")]);

    reducer.apply(&done(3, "c2", FS_READ, "new"));
    assert_eq!(
        instance(&reducer, 1, PatternId::FILE_READ_BURST)?
            .state
            .counts
            .total,
        2
    );
    reducer.apply(&done(4, "c3", FS_READ, "old"));
    assert!(
        reducer
            .instance(&id(4, PatternId::FILE_READ_BURST))
            .is_some()
    );

    let refused = reducer.apply(&rebase(5, "root", path("old"), path("new")));
    let ids: Vec<ProjectionId> = refused.iter().map(|update| update.id).collect();
    assert_eq!(
        ids,
        vec![
            id(1, PatternId::FILE_READ_BURST),
            id(5, PatternId::STANDALONE)
        ]
    );
    assert_eq!(
        refused[1].instance.state.anomaly,
        Some(Anomaly::RebaseConflict)
    );
    // `new` was replaced by the rename: its burst is over ...
    assert!(refused[0].instance.is_terminal());
    // ... while `old` is not rebound onto it.
    let old = instance(&reducer, 4, PatternId::FILE_READ_BURST)?;
    assert_eq!(old.key, Some(path("old")));
    assert!(!old.state.sealed);
    reducer.apply(&done(6, "c4", FS_READ, "new"));
    assert!(
        reducer
            .instance(&id(6, PatternId::FILE_READ_BURST))
            .is_some()
    );
    Ok(())
}

#[test]
fn rename_after_its_move_call_rebinds_the_loop_and_untouched_renames_are_plain() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&done(1, "c1", FS_READ, "old"));
    reducer.apply(&done(2, "c2", FS_READ, "new"));
    // The move call itself is a mutation of `old`: it seals the read burst
    // and opens the loop the rebase rebinds.
    let moved = reducer.apply(&done(3, "c3", FS_MOVE, "old"));
    assert!(moved[0].instance.is_terminal());
    assert_eq!(moved[1].id, id(3, PatternId::FILE_EDIT_READ_LOOP));

    // A pure binding without call id; `new` had an open burst, so this is
    // the overwrite case: refused and `new`'s burst sealed.
    let refused = reducer.apply(&rebase(4, "root", path("old"), path("new")));
    let ids: Vec<ProjectionId> = refused.iter().map(|update| update.id).collect();
    assert_eq!(
        ids,
        vec![
            id(2, PatternId::FILE_READ_BURST),
            id(4, PatternId::STANDALONE)
        ]
    );
    assert!(refused[0].instance.is_terminal());

    // Without a competing `new`, the loop moves with the file.
    reducer.apply(&done(5, "c4", FS_MOVE, "a"));
    let rebound = reducer.apply(&rebase(6, "root", path("a"), path("b")));
    assert_eq!(rebound.len(), 1);
    assert_eq!(rebound[0].id, id(5, PatternId::FILE_EDIT_READ_LOOP));
    assert_eq!(rebound[0].instance.key, Some(path("b")));
    // The rebase is provenance, not a constituent.
    assert_eq!(rebound[0].instance.state.counts.total, 1);
    assert_eq!(rebound[0].instance.constituents, vec![5, 6]);
    reducer.apply(&done(7, "c5", FS_READ, "b"));
    assert_eq!(
        verbs(instance(&reducer, 5, PatternId::FILE_EDIT_READ_LOOP)?),
        vec![("move".to_owned(), 1), ("read".to_owned(), 1)]
    );

    // Renaming a file this turn never touched is a normal event.
    let plain = reducer.apply(&rebase(8, "root", path("x"), path("y")));
    assert_eq!(plain.len(), 1);
    assert_eq!(plain[0].instance.pattern, PatternId::STANDALONE);
    assert_eq!(plain[0].instance.state.anomaly, None);
    Ok(())
}

#[test]
fn non_advancing_events_are_refused_and_change_nothing() -> TestResult {
    let mut reducer = Reducer::new();
    reducer.apply(&done(5, "c1", FS_READ, "a"));
    let before = reducer.clone();

    assert_eq!(
        reducer.try_apply(&done(5, "c2", FS_READ, "a")),
        Err(ReduceError::OutOfOrder { last: 5, got: 5 })
    );
    assert!(reducer.apply(&done(3, "c3", FS_READ, "a")).is_empty());
    assert_eq!(reducer.stale_events(), 2);
    assert_eq!(reducer.snapshot(), before.snapshot());
    assert_eq!(reducer.last_seq(), Some(5));

    // Gaps are fine (filtered streams); only the order matters.
    assert_eq!(reducer.apply(&done(9, "c4", FS_READ, "a")).len(), 1);
    let error = ReduceError::OutOfOrder { last: 9, got: 2 };
    assert_eq!(
        error.to_string(),
        "activity event ingest_seq 2 does not advance the stream (last applied 9)"
    );
    Ok(())
}

#[test]
fn a_recorded_trace_and_any_resumed_prefix_rebuild_the_live_state() -> TestResult {
    let events = vec![
        done(1, "c1", FS_EDIT, "a"),
        read(2, "c2", "a", Lifecycle::Started),
        call(
            3,
            1,
            "j1",
            JOB_START,
            Some(call_key("j1")),
            Lifecycle::Started,
        ),
        rebase(4, "root", call_key("j1"), job("W1")),
        done(5, "c2", FS_READ, "a"),
        agent_run(
            6,
            "a1",
            Some("reviewer"),
            Lifecycle::Finished,
            Some(Outcome::failed("t")),
        ),
        job_run(7, "W1", Lifecycle::Finished, Some(Outcome::Cancelled)),
        boundary(8, "root", Some(1), Boundary::TurnEnd),
    ];
    let mut live = Reducer::new();
    for event in &events {
        live.apply(event);
    }

    // The persisted log (JSONL) replays to the live state.
    let recorded = parse_jsonl(&to_jsonl(&events)?)?;
    assert_eq!(recorded, events);
    assert_eq!(Reducer::replay(&recorded), live);

    // A consumer that reconnects after any prefix and receives the rest,
    // including a re-delivery of the last event it saw, converges too.
    for split in 1..events.len() {
        let mut resumed = Reducer::replay(&recorded[..split]);
        for event in &recorded[split - 1..] {
            resumed.apply(event);
        }
        assert_eq!(resumed.snapshot(), live.snapshot(), "split {split}");
        assert_eq!(resumed.stale_events(), 1);
    }

    let cancelled = live
        .instances()
        .find(|instance| instance.pattern == PatternId::JOB_LIFECYCLE)
        .map(|instance| instance.state.counts.cancelled);
    assert_eq!(cancelled, Some(1));
    Ok(())
}

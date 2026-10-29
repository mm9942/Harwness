//! Replay tests over recorded JSONL traces (`tests/fixtures/*.jsonl`) and
//! generated streams.
//!
//! Every trace is checked for
//! - replay equality (live `apply` == `Reducer::replay` == replay of the
//!   re-serialized trace),
//! - consumer reconstruction (keeping the newest revision per projection id
//!   rebuilds the final snapshot),
//! - the PL-71/07 invariants: `compact.failed == sum(constituent.failed)`,
//!   consistent phases, no hidden event (a failure reported on any step
//!   included), unknown descriptors in no grouping pattern (failure bursts
//!   included), one work id per job instance and a consistent
//!   `job_projection`, stable projection ids, revisions taken from
//!   `ingest_seq`,
//!
//! and each fixture additionally against its hand-derived expected summary.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;

use harw_activity::{
    AGENT_DOMAIN, ActivityDescriptor, ActivityEvent, ActivityKey, ActivityOwner, Anomaly, Boundary,
    ConstituentId, Counts, Effect, Lifecycle, Outcome, PatternId, PatternInstance, Phase,
    ProjectionId, ProjectionUpdate, Reducer, parse_jsonl, to_jsonl,
};

type TestResult = Result<(), Box<dyn Error>>;

const FILE_READ_BURST: &str = include_str!("fixtures/file_read_burst.jsonl");
const FILE_EDIT_READ_LOOP: &str = include_str!("fixtures/file_edit_read_loop.jsonl");
const JOB_LIFECYCLE: &str = include_str!("fixtures/job_lifecycle.jsonl");
const AGENT_ROLE_FAILURE_BURST: &str = include_str!("fixtures/agent_role_failure_burst.jsonl");
const UNKNOWN_AND_BREAKS: &str = include_str!("fixtures/unknown_and_breaks.jsonl");
const REBASE_CONFLICT_AND_RENAME: &str = include_str!("fixtures/rebase_conflict_and_rename.jsonl");
const REDELIVERY: &str = include_str!("fixtures/redelivery.jsonl");

const ALL_FIXTURES: [(&str, &str); 7] = [
    ("file_read_burst", FILE_READ_BURST),
    ("file_edit_read_loop", FILE_EDIT_READ_LOOP),
    ("job_lifecycle", JOB_LIFECYCLE),
    ("agent_role_failure_burst", AGENT_ROLE_FAILURE_BURST),
    ("unknown_and_breaks", UNKNOWN_AND_BREAKS),
    ("rebase_conflict_and_rename", REBASE_CONFLICT_AND_RENAME),
    ("redelivery", REDELIVERY),
];

// ── harness ─────────────────────────────────────────────────────────────────

/// Result of reducing one trace live, update by update.
struct Run {
    reducer: Reducer,
    /// `(ingest_seq, update)` for every emitted update.
    updates: Vec<(u64, ProjectionUpdate)>,
}

fn run_live(events: &[ActivityEvent]) -> Run {
    let mut reducer = Reducer::new();
    let mut updates = Vec::new();
    for event in events {
        for update in reducer.apply(event) {
            updates.push((event.ingest_seq, update));
        }
    }
    Run { reducer, updates }
}

/// One line per instance: `id key phase counts verbs [aliases] [anomaly]`.
fn summary(instance: &PatternInstance) -> String {
    let key = instance
        .key
        .as_ref()
        .map_or_else(|| "-".to_owned(), ToString::to_string);
    let phase = match instance.state.phase {
        Phase::Open => "open",
        Phase::Quiescent => "quiescent",
        Phase::Terminal => "terminal",
    };
    let counts = instance.state.counts;
    let verbs: Vec<String> = instance
        .state
        .verbs
        .iter()
        .map(|tally| format!("{}x{}", tally.verb, tally.counts.total))
        .collect();
    let mut line = format!(
        "{} {key} {phase} total={} running={} failed={} verbs={}",
        instance.id,
        counts.total,
        counts.running,
        counts.failed,
        verbs.join(",")
    );
    if !instance.state.aliases.is_empty() {
        let aliases: Vec<String> = instance
            .state
            .aliases
            .iter()
            .map(ToString::to_string)
            .collect();
        line.push_str(&format!(" aliases={}", aliases.join(",")));
    }
    if let Some(anomaly) = instance.state.anomaly {
        let name = match anomaly {
            Anomaly::RebaseUnmatched => "rebase-unmatched",
            Anomaly::RebaseConflict => "rebase-conflict",
            Anomaly::RebaseFailed => "rebase-failed",
            Anomaly::Unpaired => "unpaired",
        };
        line.push_str(&format!(" anomaly={name}"));
    }
    line
}

fn summaries(reducer: &Reducer) -> Vec<String> {
    reducer.instances().map(summary).collect()
}

fn is_failure(outcome: Option<&Outcome>) -> bool {
    matches!(outcome, Some(Outcome::Failed { .. }))
}

fn is_agent_run(descriptor: &ActivityDescriptor) -> bool {
    descriptor.is_known() && descriptor.domain == AGENT_DOMAIN
}

fn recount<'a>(statuses: impl Iterator<Item = &'a harw_activity::ConstituentStatus>) -> Counts {
    let mut counts = Counts::default();
    for status in statuses {
        counts.total += 1;
        match status {
            harw_activity::ConstituentStatus::Running => counts.running += 1,
            harw_activity::ConstituentStatus::Succeeded => counts.succeeded += 1,
            harw_activity::ConstituentStatus::Failed { .. } => counts.failed += 1,
            harw_activity::ConstituentStatus::Cancelled => counts.cancelled += 1,
        }
    }
    counts
}

/// Checks every structural invariant of one reduction; returns the first
/// violation as an error message.
fn check_invariants(events: &[ActivityEvent], run: &Run) -> Result<(), String> {
    let reducer = &run.reducer;

    // Applied events: strictly increasing ingest_seq, the rest is stale.
    let mut applied: Vec<&ActivityEvent> = Vec::new();
    let mut last = None;
    for event in events {
        if last.is_none_or(|last| event.ingest_seq > last) {
            last = Some(event.ingest_seq);
            applied.push(event);
        }
    }
    let stale = u64::try_from(events.len() - applied.len()).map_err(|e| e.to_string())?;
    if reducer.stale_events() != stale {
        return Err(format!(
            "stale count {} != expected {stale}",
            reducer.stale_events()
        ));
    }
    let last_seq = reducer.last_seq().unwrap_or(0);
    let by_seq: BTreeMap<u64, &ActivityEvent> = applied
        .iter()
        .map(|event| (event.ingest_seq, *event))
        .collect();

    let mut covered: BTreeSet<u64> = BTreeSet::new();
    for instance in reducer.instances() {
        let name = instance.id.to_string();
        let state = &instance.state;
        if instance.id.pattern != instance.pattern
            || PatternId::parse(instance.pattern.as_str()).is_none()
        {
            return Err(format!("{name}: inconsistent pattern id"));
        }
        // Compact failure invariant and counter consistency.
        let counts = recount(state.members.iter().map(|member| &member.status));
        if counts != state.counts {
            return Err(format!(
                "{name}: counts {:?} != members {counts:?}",
                state.counts
            ));
        }
        let failed: u64 = state.members.iter().map(|member| member.failed()).sum();
        if state.counts.failed != failed {
            return Err(format!("{name}: compact.failed != sum(constituent.failed)"));
        }
        let mut by_verbs = Counts::default();
        for tally in &state.verbs {
            by_verbs.total += tally.counts.total;
            by_verbs.running += tally.counts.running;
            by_verbs.succeeded += tally.counts.succeeded;
            by_verbs.failed += tally.counts.failed;
            by_verbs.cancelled += tally.counts.cancelled;
        }
        if by_verbs != state.counts {
            return Err(format!("{name}: verb tallies do not add up"));
        }
        let verbs: BTreeSet<&str> = state
            .verbs
            .iter()
            .map(|tally| tally.verb.as_ref())
            .collect();
        if verbs.len() != state.verbs.len() {
            return Err(format!("{name}: duplicate verb tally"));
        }
        // Phase.
        let phase = if state.counts.running > 0 {
            Phase::Open
        } else if state.sealed {
            Phase::Terminal
        } else {
            Phase::Quiescent
        };
        if phase != state.phase {
            return Err(format!(
                "{name}: phase {:?} != derived {phase:?}",
                state.phase
            ));
        }
        // Provenance and revision.
        if !instance
            .constituents
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        {
            return Err(format!("{name}: constituents not ascending"));
        }
        if instance
            .constituents
            .last()
            .is_some_and(|seq| *seq > instance.revision)
            || instance.revision > last_seq
            || instance.revision < instance.id.opened_at
        {
            return Err(format!(
                "{name}: revision {} out of range",
                instance.revision
            ));
        }
        covered.extend(instance.constituents.iter().copied());
        // Distinct members.
        let ids: BTreeSet<&ConstituentId> = state.members.iter().map(|member| &member.id).collect();
        if ids.len() != state.members.len() {
            return Err(format!("{name}: duplicate constituent"));
        }
        // Pattern-specific shape.
        if instance.pattern == PatternId::STANDALONE && (!state.sealed || state.members.len() != 1)
        {
            return Err(format!("{name}: standalone must be sealed with one member"));
        }
        if instance.pattern == PatternId::AGENT_ROLE_FAILURE_BURST
            && state.members.iter().any(|member| {
                !member.status.is_failed()
                    || !matches!(member.id, ConstituentId::Key(ActivityKey::Agent(_)))
            })
        {
            return Err(format!(
                "{name}: failure burst with a non-failed or non-agent member"
            ));
        }
        if state.anomaly.is_some() && instance.pattern != PatternId::STANDALONE {
            return Err(format!("{name}: anomaly outside a standalone instance"));
        }
        if state.anomaly == Some(Anomaly::Unpaired)
            && !state.members.iter().all(|member| {
                matches!(member.id, ConstituentId::Event(_)) && member.status.is_running()
            })
        {
            return Err(format!("{name}: unpaired flag on a pairable activity"));
        }
        // I5: nothing first seen with unknown semantics joins a grouping
        // pattern, failure bursts included; bursts cover agent runs only.
        if instance.pattern != PatternId::STANDALONE {
            for member in &state.members {
                let introduced_unknown = by_seq.get(&member.first_seq).is_some_and(|event| {
                    ConstituentId::of(event) == member.id && !event.descriptor.is_known()
                });
                if !member.is_known() || introduced_unknown {
                    return Err(format!(
                        "{name}: unknown constituent {:?} grouped",
                        member.id
                    ));
                }
                if instance.pattern == PatternId::AGENT_ROLE_FAILURE_BURST
                    && member.domain != AGENT_DOMAIN
                {
                    return Err(format!("{name}: non-agent constituent in a failure burst"));
                }
            }
        }
        // One work id per job instance, and it is the instance key.
        if instance.pattern == PatternId::JOB_LIFECYCLE {
            let runs: Vec<&ActivityKey> = state
                .members
                .iter()
                .filter_map(|member| match &member.id {
                    ConstituentId::Key(key @ ActivityKey::Job(_)) => Some(key),
                    _ => None,
                })
                .collect();
            if runs.len() > 1 {
                return Err(format!("{name}: job instance with work ids {runs:?}"));
            }
            if runs
                .first()
                .is_some_and(|run| instance.key.as_ref() != Some(*run))
            {
                return Err(format!("{name}: run {runs:?} is not the instance key"));
            }
        }
    }

    // job_projection(w) always names the job instance keyed by w.
    let work_ids: BTreeSet<&str> = applied
        .iter()
        .flat_map(|event| {
            let to = match &event.lifecycle {
                Lifecycle::Rebase { to } => Some(to),
                _ => None,
            };
            [event.resource.as_ref(), to]
        })
        .flatten()
        .filter_map(|key| match key {
            ActivityKey::Job(work) => Some(work.as_str()),
            _ => None,
        })
        .collect();
    for work in work_ids {
        let Some(id) = reducer.job_projection(work) else {
            continue;
        };
        let keyed = reducer.instance(&id).is_some_and(|instance| {
            instance.pattern == PatternId::JOB_LIFECYCLE
                && instance.key == Some(ActivityKey::Job(work.to_owned()))
        });
        if !keyed {
            return Err(format!(
                "job_projection({work}) = {id} is keyed differently"
            ));
        }
    }

    for event in &applied {
        let is_boundary = matches!(event.lifecycle, Lifecycle::Boundary(_));
        // I1: no applied activity is hidden.
        if !is_boundary && !covered.contains(&event.ingest_seq) {
            return Err(format!("event {} is in no instance", event.ingest_seq));
        }
        // A rebase that reports a failure keeps it visible (fail closed).
        if matches!(event.lifecycle, Lifecycle::Rebase { .. }) && is_failure(event.outcome.as_ref())
        {
            let point = ConstituentId::Event(event.ingest_seq);
            let visible = reducer.instances().any(|instance| {
                instance.failed() > 0
                    && instance
                        .member(&point)
                        .is_some_and(|member| member.status.is_failed())
            });
            if !visible {
                return Err(format!("event {}: failed rebase hidden", event.ingest_seq));
            }
        }
        if !matches!(
            event.lifecycle,
            Lifecycle::Started | Lifecycle::Progress | Lifecycle::Finished
        ) {
            continue;
        }
        let member = ConstituentId::of(event);
        let Some(home_id) = reducer.projection_of(&member) else {
            return Err(format!(
                "event {}: constituent has no home",
                event.ingest_seq
            ));
        };
        let Some(home) = reducer.instance(&home_id) else {
            return Err(format!(
                "event {}: home {home_id} missing",
                event.ingest_seq
            ));
        };
        let Some(constituent) = home.member(&member) else {
            return Err(format!(
                "event {}: not a member of its home",
                event.ingest_seq
            ));
        };
        // I5: unknown semantics never join a grouping pattern.
        if constituent.first_seq == event.ingest_seq
            && !event.descriptor.is_known()
            && home.pattern != PatternId::STANDALONE
        {
            return Err(format!(
                "event {}: unknown descriptor grouped",
                event.ingest_seq
            ));
        }
        // Failures stay visible in the compact state of the home, whatever
        // lifecycle step reported them.
        let failed_here = is_failure(event.outcome.as_ref());
        if failed_here && (!constituent.status.is_failed() || home.failed() == 0) {
            return Err(format!("event {}: failure hidden", event.ingest_seq));
        }
        // A failed agent run (known agent semantics on both the reporting
        // event and the run itself) is covered by a failure burst.
        if failed_here
            && matches!(member, ConstituentId::Key(ActivityKey::Agent(_)))
            && is_agent_run(&event.descriptor)
            && constituent.is_known()
            && constituent.domain == AGENT_DOMAIN
        {
            let burst = reducer
                .failure_burst_of(&member)
                .and_then(|id| reducer.instance(&id));
            if !burst.is_some_and(|burst| burst.member(&member).is_some() && burst.failed() > 0) {
                return Err(format!(
                    "event {}: failed agent run not in a burst",
                    event.ingest_seq
                ));
            }
        }
    }

    // Update stream: revisions come from ingest_seq, ids are stable, and the
    // newest revision per id reconstructs the snapshot.
    let mut view: BTreeMap<ProjectionId, PatternInstance> = BTreeMap::new();
    for (seq, update) in &run.updates {
        if update.revision != *seq || update.instance.revision != *seq {
            return Err(format!(
                "update for {} at {seq} has revision {}",
                update.id, update.revision
            ));
        }
        if update.instance.id != update.id {
            return Err(format!("update id {} != instance id", update.id));
        }
        if view.get(&update.id).is_some_and(|previous| {
            previous.revision >= update.revision || previous.pattern != update.instance.pattern
        }) {
            return Err(format!(
                "{}: non-monotonic revision or pattern change",
                update.id
            ));
        }
        view.insert(update.id, update.instance.clone());
    }
    let view: Vec<PatternInstance> = view.into_values().collect();
    if view != reducer.snapshot() {
        return Err("consumer view rebuilt from updates != snapshot".to_owned());
    }
    Ok(())
}

/// Live reduction, replay, re-serialized replay and all invariants.
fn reduce_and_check(name: &str, events: &[ActivityEvent]) -> Result<Run, Box<dyn Error>> {
    let run = run_live(events);
    check_invariants(events, &run).map_err(|message| format!("{name}: {message}"))?;
    if Reducer::replay(events) != run.reducer {
        return Err(format!("{name}: replay != live").into());
    }
    let reparsed = parse_jsonl(&to_jsonl(events)?)?;
    if reparsed != events {
        return Err(format!("{name}: trace does not round-trip").into());
    }
    if Reducer::replay(&reparsed) != run.reducer {
        return Err(format!("{name}: replay of re-serialized trace != live").into());
    }
    for (_, update) in &run.updates {
        let json = serde_json::to_string(update)?;
        let back: ProjectionUpdate = serde_json::from_str(&json)?;
        if &back != update {
            return Err(format!("{name}: projection update does not round-trip").into());
        }
    }
    Ok(run)
}

fn fixture(trace: &str) -> Result<(Vec<ActivityEvent>, Run), Box<dyn Error>> {
    let events = parse_jsonl(trace)?;
    let run = reduce_and_check("fixture", &events)?;
    Ok((events, run))
}

// ── fixtures ────────────────────────────────────────────────────────────────

#[test]
fn every_fixture_replays_identically_and_keeps_the_invariants() -> TestResult {
    for (name, trace) in ALL_FIXTURES {
        let events = parse_jsonl(trace)?;
        assert!(!events.is_empty(), "{name}: empty fixture");
        reduce_and_check(name, &events)?;
    }
    Ok(())
}

#[test]
fn fixture_file_read_burst() -> TestResult {
    let (_, run) = fixture(FILE_READ_BURST)?;
    assert_eq!(
        summaries(&run.reducer),
        vec![
            "file-read-burst@1#1 path:src/lib.rs terminal total=3 running=0 failed=1 verbs=readx2,searchx1",
            "file-read-burst@1#4 path:src/main.rs terminal total=1 running=0 failed=0 verbs=readx1",
            "file-read-burst@1#9 path:src/lib.rs quiescent total=1 running=0 failed=0 verbs=readx1",
        ]
    );
    // Read(A)#1 and Read(A)#2 were running at the same time in one instance.
    let overlap = run
        .updates
        .iter()
        .find(|(seq, _)| *seq == 2)
        .map(|(_, update)| update.instance.state.counts.running);
    assert_eq!(overlap, Some(2));
    let burst = run.reducer.instances().next().ok_or("no instance")?;
    assert_eq!(burst.constituents, vec![1, 2, 3, 5, 7]);
    assert_eq!(burst.revision, 8);
    Ok(())
}

#[test]
fn fixture_file_edit_read_loop() -> TestResult {
    let (_, run) = fixture(FILE_EDIT_READ_LOOP)?;
    assert_eq!(
        summaries(&run.reducer),
        vec![
            "file-edit-read-loop@1#1 path:src/lib.rs terminal total=4 running=0 failed=0 verbs=editx1,readx3",
            "file-read-burst@1#8 path:README.md terminal total=1 running=0 failed=0 verbs=readx1",
            "file-edit-read-loop@1#10 path:README.md terminal total=2 running=0 failed=1 verbs=writex1,readx1",
        ]
    );
    let write_loop = run
        .reducer
        .instance(&ProjectionId {
            opened_at: 10,
            pattern: PatternId::FILE_EDIT_READ_LOOP,
        })
        .ok_or("no write loop")?;
    // The failed constituent is the mutating one, and it stays visible.
    assert_eq!(write_loop.state.verbs[0].counts.failed, 1);
    assert_eq!(write_loop.state.members[0].effect, Effect::Mutate);
    Ok(())
}

#[test]
fn fixture_job_lifecycle() -> TestResult {
    let (_, run) = fixture(JOB_LIFECYCLE)?;
    assert_eq!(
        summaries(&run.reducer),
        vec![
            "job-lifecycle@1#1 job:W1 terminal total=5 running=0 failed=1 verbs=startx1,runx1,statusx1,logsx1,waitx1 aliases=call:c1",
            "job-lifecycle@1#10 job:W2 terminal total=2 running=0 failed=0 verbs=startx1,runx1 aliases=call:c5",
            "standalone@1#15 job:W1 terminal total=1 running=0 failed=0 verbs=logsx1",
        ]
    );
    // The rebase keeps the projection: every update touching W1 before its
    // end (events 1..=14) carries the same id.
    let w1 = ProjectionId {
        opened_at: 1,
        pattern: PatternId::JOB_LIFECYCLE,
    };
    let ids: BTreeSet<ProjectionId> = run
        .updates
        .iter()
        .filter(|(seq, update)| {
            *seq <= 14 && update.instance.key == Some(ActivityKey::Job("W1".into()))
        })
        .map(|(_, update)| update.id)
        .collect();
    assert_eq!(ids.into_iter().collect::<Vec<_>>(), vec![w1]);
    let before_rebase = run
        .updates
        .iter()
        .find(|(seq, _)| *seq == 1)
        .map(|(_, update)| (update.id, update.instance.key.clone()));
    assert_eq!(
        before_rebase,
        Some((w1, Some(ActivityKey::Call("c1".into()))))
    );
    assert_eq!(run.reducer.job_projection("W1"), Some(w1));
    // The run ended while job.wait was still running: sealed but open, then
    // terminal with the wait's completion.
    let at_run_end = run
        .updates
        .iter()
        .find(|(seq, _)| *seq == 13)
        .map(|(_, update)| (update.instance.state.sealed, update.instance.state.phase));
    assert_eq!(at_run_end, Some((true, Phase::Open)));
    Ok(())
}

#[test]
fn fixture_agent_role_failure_burst() -> TestResult {
    let (_, run) = fixture(AGENT_ROLE_FAILURE_BURST)?;
    assert_eq!(
        summaries(&run.reducer),
        vec![
            "standalone@1#1 agent:a1 terminal total=1 running=0 failed=1 verbs=runx1",
            "standalone@1#2 agent:a2 terminal total=1 running=0 failed=1 verbs=runx1",
            "standalone@1#3 agent:a3 terminal total=1 running=0 failed=1 verbs=runx1",
            "standalone@1#4 agent:e1 open total=1 running=1 failed=0 verbs=runx1",
            "agent-role-failure-burst@1#5 - terminal total=3 running=0 failed=3 verbs=runx3",
            "standalone@1#7 agent:a4 terminal total=1 running=0 failed=1 verbs=runx1",
            "agent-role-failure-burst@1#8 - terminal total=1 running=0 failed=1 verbs=runx1",
            "agent-role-failure-burst@1#10 - terminal total=1 running=0 failed=1 verbs=runx1",
            "standalone@1#10 agent:a5 terminal total=1 running=0 failed=1 verbs=runx1",
            "standalone@1#11 agent:a6 terminal total=1 running=0 failed=0 verbs=runx1",
            "agent-role-failure-burst@1#13 - quiescent total=1 running=0 failed=1 verbs=runx1",
            "standalone@1#13 agent:a7 terminal total=1 running=0 failed=1 verbs=runx1",
        ]
    );
    // The burst exposes the concrete agent ids (I2/I6).
    let burst = run
        .reducer
        .instance(&ProjectionId {
            opened_at: 5,
            pattern: PatternId::AGENT_ROLE_FAILURE_BURST,
        })
        .ok_or("no burst")?;
    let agents: Vec<String> = burst
        .state
        .members
        .iter()
        .filter_map(|member| match &member.id {
            ConstituentId::Key(key) => Some(key.to_string()),
            _ => None,
        })
        .collect();
    assert_eq!(agents, vec!["agent:a1", "agent:a2", "agent:a3"]);
    Ok(())
}

#[test]
fn fixture_unknown_and_breaks() -> TestResult {
    let (_, run) = fixture(UNKNOWN_AND_BREAKS)?;
    assert_eq!(
        summaries(&run.reducer),
        vec![
            "file-read-burst@1#1 path:a.rs terminal total=1 running=0 failed=0 verbs=readx1",
            "standalone@1#3 - terminal total=1 running=0 failed=1 verbs=unknownx1",
            "file-read-burst@1#5 path:a.rs terminal total=1 running=0 failed=0 verbs=readx1",
            "standalone@1#6 - terminal total=1 running=0 failed=0 verbs=execx1",
            "file-read-burst@1#7 path:a.rs terminal total=2 running=0 failed=0 verbs=readx2",
            "file-edit-read-loop@1#8 path:b.rs terminal total=1 running=0 failed=0 verbs=writex1",
            "standalone@1#10 path:a.rs terminal total=1 running=0 failed=0 verbs=chmodx1",
            "standalone@1#11 other:docs.rs terminal total=1 running=0 failed=0 verbs=fetchx1",
            "file-read-burst@1#12 path:a.rs terminal total=1 running=0 failed=0 verbs=readx1",
            "standalone@1#13 path:a.rs terminal total=1 running=0 failed=0 verbs=deletex1",
            "standalone@1#14 call:zz terminal total=1 running=0 failed=0 verbs=startx1 anomaly=rebase-unmatched",
        ]
    );
    Ok(())
}

#[test]
fn fixture_rebase_conflict_and_rename() -> TestResult {
    let (_, run) = fixture(REBASE_CONFLICT_AND_RENAME)?;
    assert_eq!(
        summaries(&run.reducer),
        vec![
            "job-lifecycle@1#1 job:W1 open total=2 running=1 failed=0 verbs=startx1,runx1 aliases=call:c1",
            "job-lifecycle@1#2 call:c2 quiescent total=1 running=0 failed=0 verbs=startx1",
            "standalone@1#4 call:c2 terminal total=1 running=0 failed=0 verbs=startx1 anomaly=rebase-conflict",
            "job-lifecycle@1#7 job:W3 open total=2 running=2 failed=0 verbs=startx1,runx1 aliases=call:c1",
            "file-read-burst@1#9 path:old.rs terminal total=1 running=0 failed=0 verbs=readx1",
            "file-edit-read-loop@1#10 path:new.rs terminal total=2 running=0 failed=0 verbs=movex1,readx1 aliases=path:old.rs",
            "file-read-burst@1#13 path:old.rs quiescent total=1 running=0 failed=0 verbs=readx1",
            "file-read-burst@1#14 path:other.rs terminal total=1 running=0 failed=0 verbs=readx1",
            "file-edit-read-loop@1#15 path:other.rs quiescent total=1 running=0 failed=0 verbs=movex1",
            "standalone@1#16 path:other.rs terminal total=1 running=0 failed=0 verbs=movex1 anomaly=rebase-conflict",
            "file-read-burst@1#17 path:new.rs quiescent total=1 running=0 failed=0 verbs=readx1",
            "standalone@1#18 path:gone.rs terminal total=1 running=0 failed=0 verbs=movex1",
        ]
    );
    // The rename is a pure binding: provenance of the loop, not a member.
    let moved = run
        .reducer
        .instance(&ProjectionId {
            opened_at: 10,
            pattern: PatternId::FILE_EDIT_READ_LOOP,
        })
        .ok_or("no moved loop")?;
    assert_eq!(moved.constituents, vec![10, 11, 12]);
    // Sealed by the overwrite at 16.
    assert_eq!(moved.revision, 16);
    Ok(())
}

#[test]
fn fixture_redelivery_is_idempotent() -> TestResult {
    let (events, run) = fixture(REDELIVERY)?;
    assert_eq!(run.reducer.stale_events(), 2);
    assert_eq!(
        summaries(&run.reducer),
        vec!["file-read-burst@1#1 path:a.rs quiescent total=2 running=0 failed=1 verbs=readx2"]
    );
    let mut seen = BTreeSet::new();
    let deduplicated: Vec<ActivityEvent> = events
        .iter()
        .filter(|event| seen.insert(event.ingest_seq))
        .cloned()
        .collect();
    assert_eq!(deduplicated.len(), 4);
    assert_eq!(
        Reducer::replay(&deduplicated).snapshot(),
        run.reducer.snapshot()
    );
    Ok(())
}

// ── generated streams ───────────────────────────────────────────────────────

/// Small deterministic generator (64-bit LCG); no external test dependency.
struct Lcg(u64);

impl Lcg {
    fn step(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.step()).unwrap_or(0) % n.max(1)
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        items.get(self.below(items.len()))
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.step() % 100 < percent
    }
}

const DESCRIPTORS: [ActivityDescriptor; 10] = [
    ActivityDescriptor::new("filesystem", "read", Some("path"), Effect::Observe),
    ActivityDescriptor::new("filesystem", "search", Some("path"), Effect::Observe),
    ActivityDescriptor::new("filesystem", "edit", Some("path"), Effect::Mutate),
    ActivityDescriptor::new("filesystem", "create", Some("path"), Effect::Create),
    ActivityDescriptor::new("filesystem", "delete", Some("path"), Effect::Delete),
    ActivityDescriptor::new("process", "exec", None, Effect::Execute),
    ActivityDescriptor::new("network", "fetch", None, Effect::Communicate),
    ActivityDescriptor::UNKNOWN,
    ActivityDescriptor::new("job", "start", None, Effect::Execute),
    ActivityDescriptor::new("job", "status", Some("work_id"), Effect::Observe),
];

const FS_MOVE: ActivityDescriptor =
    ActivityDescriptor::new("filesystem", "move", Some("path"), Effect::Mutate);
const MESSAGE: ActivityDescriptor =
    ActivityDescriptor::new("messaging", "send", Some("to"), Effect::Communicate);

/// An activity that has started and may still progress/finish.
#[derive(Clone)]
struct OpenActivity {
    owner: ActivityOwner,
    descriptor: ActivityDescriptor,
    resource: Option<ActivityKey>,
}

fn generate(seed: u64, len: usize) -> Vec<ActivityEvent> {
    let mut rng = Lcg(seed);
    let sessions = ["root", "child"];
    let paths = ["a", "b", "c"];
    let roles = [Some("reviewer"), Some("explorer"), None];
    let classes = [Some("timeout"), Some("crash"), None];
    let mut turns: BTreeMap<&str, u64> = BTreeMap::new();
    let mut open: Vec<OpenActivity> = Vec::new();
    let mut job_calls: Vec<(String, String)> = Vec::new();
    let mut work_ids: Vec<String> = Vec::new();
    let mut events: Vec<ActivityEvent> = Vec::new();
    let mut seq = 0_u64;
    let mut next_call = 0_u64;

    while events.len() < len {
        // Occasional re-delivery of an earlier event (must be refused).
        if !events.is_empty() && rng.chance(4) {
            if let Some(old) = rng.pick(&events).cloned() {
                events.push(old);
            }
            continue;
        }
        seq += 1 + u64::from(rng.chance(10));
        let session = *rng.pick(&sessions).unwrap_or(&"root");
        let turn = *turns.entry(session).or_insert(1);
        let roll = rng.below(100);
        let event = if roll < 35 {
            // New tool call.
            next_call += 1;
            let call = format!("k{next_call}");
            let descriptor = rng
                .pick(&DESCRIPTORS)
                .cloned()
                .unwrap_or(ActivityDescriptor::UNKNOWN);
            let resource = match (descriptor.domain.as_ref(), descriptor.verb.as_ref()) {
                ("job", "start") => {
                    job_calls.push((session.to_owned(), call.clone()));
                    Some(ActivityKey::Call(call.clone()))
                }
                ("job", _) => Some(ActivityKey::Job(
                    rng.pick(&work_ids)
                        .cloned()
                        .unwrap_or_else(|| "W0".to_owned()),
                )),
                ("filesystem", _) => Some(ActivityKey::Path(
                    (*rng.pick(&paths).unwrap_or(&"a")).to_owned(),
                )),
                _ if rng.chance(30) => Some(ActivityKey::Path(
                    (*rng.pick(&paths).unwrap_or(&"a")).to_owned(),
                )),
                _ => None,
            };
            let mut owner = ActivityOwner::session(session)
                .with_turn(turn)
                .with_call(call);
            if rng.chance(3) {
                // Adapter bug: no call id, so nothing can pair it.
                owner.call = None;
            }
            if rng.chance(25) {
                ActivityEvent::new(seq, owner, descriptor, Lifecycle::Finished)
                    .with_resource_opt(resource)
                    .with_outcome(random_outcome(&mut rng, &classes))
            } else {
                open.push(OpenActivity {
                    owner: owner.clone(),
                    descriptor: descriptor.clone(),
                    resource: resource.clone(),
                });
                ActivityEvent::new(seq, owner, descriptor, Lifecycle::Started)
                    .with_resource_opt(resource)
            }
        } else if roll < 65 && !open.is_empty() {
            // Progress or finish an open activity.
            let index = rng.below(open.len());
            let activity = open[index].clone();
            if rng.chance(20) {
                let mut event = ActivityEvent::new(
                    seq,
                    activity.owner,
                    activity.descriptor,
                    Lifecycle::Progress,
                )
                .with_resource_opt(activity.resource);
                if rng.chance(15) {
                    // A failure reported before the end must count too.
                    event.outcome = Some(Outcome::failed("io"));
                }
                event
            } else {
                open.remove(index);
                ActivityEvent::new(
                    seq,
                    activity.owner,
                    activity.descriptor,
                    Lifecycle::Finished,
                )
                .with_resource_opt(activity.resource)
                .with_outcome(random_outcome(&mut rng, &classes))
            }
        } else if roll < 72 && !job_calls.is_empty() {
            // Rebase a job.start call; sometimes onto an already used WorkId,
            // sometimes a call twice, from an unknown call, or `Job` -> `Job`.
            let (caller, call) = job_calls[rng.below(job_calls.len())].clone();
            let work = if rng.chance(30) {
                rng.pick(&work_ids)
                    .cloned()
                    .unwrap_or_else(|| "W0".to_owned())
            } else {
                format!("W{seq}")
            };
            let from = match rng.below(10) {
                0 => ActivityKey::Call(format!("ghost{seq}")),
                1 => ActivityKey::Job(
                    rng.pick(&work_ids)
                        .cloned()
                        .unwrap_or_else(|| "W0".to_owned()),
                ),
                _ => ActivityKey::Call(call.clone()),
            };
            work_ids.push(work.clone());
            let mut event = ActivityEvent::new(
                seq,
                ActivityOwner::session(caller)
                    .with_turn(turn)
                    .with_call(call),
                DESCRIPTORS[8].clone(),
                Lifecycle::Rebase {
                    to: ActivityKey::Job(work),
                },
            )
            .with_resource(from);
            if rng.chance(5) {
                event.outcome = Some(Outcome::failed("spawn"));
            }
            event
        } else if roll < 77 && !work_ids.is_empty() {
            // The job runtime reports a run.
            let work = rng
                .pick(&work_ids)
                .cloned()
                .unwrap_or_else(|| "W0".to_owned());
            let lifecycle = if rng.chance(50) {
                Lifecycle::Progress
            } else {
                Lifecycle::Finished
            };
            let mut event = ActivityEvent::new(
                seq,
                ActivityOwner::session("root"),
                ActivityDescriptor::JOB_RUN,
                lifecycle,
            )
            .with_resource(ActivityKey::Job(work));
            if event.lifecycle == Lifecycle::Finished {
                event.outcome = Some(random_outcome(&mut rng, &classes));
            }
            event
        } else if roll < 87 {
            // Child agent run.
            let agent = format!("agent-{}", rng.below(6));
            let mut owner = ActivityOwner::session(agent.clone()).with_parent("root");
            owner.role = rng.pick(&roles).copied().flatten().map(str::to_owned);
            let lifecycle = match rng.below(3) {
                0 => Lifecycle::Started,
                1 => Lifecycle::Progress,
                _ => Lifecycle::Finished,
            };
            // Mostly real agent runs; sometimes unknown or foreign semantics
            // keyed by the agent, which must never reach a failure burst.
            let descriptor = match rng.below(10) {
                0 => ActivityDescriptor::UNKNOWN,
                1 => MESSAGE,
                _ => ActivityDescriptor::AGENT_RUN,
            };
            let mut event = ActivityEvent::new(seq, owner, descriptor, lifecycle)
                .with_resource(ActivityKey::Agent(agent));
            if event.lifecycle == Lifecycle::Finished {
                event.outcome = Some(random_outcome(&mut rng, &classes));
            }
            event
        } else if roll < 92 {
            // Observed rename, usually after its own move call (adapter
            // contract rule 4), sometimes as a bare binding.
            let from = (*rng.pick(&paths).unwrap_or(&"a")).to_owned();
            let to = (*rng.pick(&paths).unwrap_or(&"b")).to_owned();
            if rng.chance(60) {
                next_call += 1;
                events.push(
                    ActivityEvent::new(
                        seq,
                        ActivityOwner::session(session)
                            .with_turn(turn)
                            .with_call(format!("k{next_call}")),
                        FS_MOVE,
                        Lifecycle::Finished,
                    )
                    .with_resource(ActivityKey::Path(from.clone()))
                    .with_outcome(random_outcome(&mut rng, &classes)),
                );
                seq += 1;
            }
            ActivityEvent::new(
                seq,
                ActivityOwner::session(session).with_turn(turn),
                FS_MOVE,
                Lifecycle::Rebase {
                    to: ActivityKey::Path(to),
                },
            )
            .with_resource(ActivityKey::Path(from))
        } else {
            // Scope boundary.
            let kind = match rng.below(3) {
                0 => Boundary::TurnEnd,
                1 => Boundary::Timeout,
                _ => Boundary::SessionEnd,
            };
            if kind == Boundary::TurnEnd {
                turns.insert(session, turn + 1);
            }
            ActivityEvent::new(
                seq,
                ActivityOwner::session(session).with_turn(turn),
                ActivityDescriptor::SCOPE,
                Lifecycle::Boundary(kind),
            )
        };
        events.push(event);
    }
    events
}

fn random_outcome(rng: &mut Lcg, classes: &[Option<&str>]) -> Outcome {
    match rng.below(4) {
        0 => Outcome::Failed {
            class: rng.pick(classes).copied().flatten().map(str::to_owned),
        },
        1 => Outcome::Cancelled,
        _ => Outcome::Succeeded,
    }
}

/// `with_resource` for an optional key.
trait WithResourceOpt {
    fn with_resource_opt(self, resource: Option<ActivityKey>) -> Self;
}

impl WithResourceOpt for ActivityEvent {
    fn with_resource_opt(mut self, resource: Option<ActivityKey>) -> Self {
        self.resource = resource;
        self
    }
}

#[test]
fn generated_streams_replay_identically_and_keep_the_invariants() -> TestResult {
    for seed in 0..48_u64 {
        let events = generate(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xA5A5, 240);
        reduce_and_check(&format!("seed {seed}"), &events)?;
    }
    Ok(())
}

#[test]
fn generated_streams_exercise_every_pattern() -> TestResult {
    let mut seen: BTreeSet<PatternId> = BTreeSet::new();
    let mut anomalies: BTreeSet<Anomaly> = BTreeSet::new();
    let mut rebased_jobs = 0_usize;
    for seed in 0..48_u64 {
        let events = generate(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xA5A5, 240);
        let reducer = Reducer::replay(&events);
        for instance in reducer.instances() {
            seen.insert(instance.pattern);
            anomalies.extend(instance.state.anomaly);
            if instance.pattern == PatternId::JOB_LIFECYCLE && !instance.state.aliases.is_empty() {
                rebased_jobs += 1;
            }
        }
    }
    assert_eq!(seen.into_iter().collect::<Vec<_>>(), {
        let mut all = PatternId::ALL.to_vec();
        all.sort();
        all
    });
    assert_eq!(
        anomalies.into_iter().collect::<Vec<_>>(),
        vec![
            Anomaly::RebaseUnmatched,
            Anomaly::RebaseConflict,
            Anomaly::RebaseFailed,
            Anomaly::Unpaired
        ],
        "every anomaly must occur"
    );
    assert!(rebased_jobs > 0);
    Ok(())
}

#[test]
fn a_late_arrival_never_rewrites_earlier_state() -> TestResult {
    // Applying a prefix and then the rest equals the whole; the prefix state
    // is a strict history of the projection (revisions only grow).
    let events = generate(7, 200);
    let split = events.len() / 2;
    let mut reducer = Reducer::replay(&events[..split]);
    let prefix = reducer.snapshot();
    for event in &events[split..] {
        reducer.apply(event);
    }
    assert_eq!(reducer, Reducer::replay(&events));
    for earlier in prefix {
        let later = reducer
            .instance(&earlier.id)
            .ok_or("instance disappeared")?;
        assert!(later.revision >= earlier.revision);
        assert!(later.constituents.starts_with(&earlier.constituents));
        assert!(later.state.members.len() >= earlier.state.members.len());
        assert!(later.failed() >= earlier.failed(), "a failure vanished");
    }
    Ok(())
}

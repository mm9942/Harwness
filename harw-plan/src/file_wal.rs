//! Bounded transaction-framed plan history with a legacy read path.
//!
//! Each new batch occupies exactly one LF-terminated JSON line. An
//! unterminated suffix is not a committed record and is never partially
//! applied. The digest detects corruption; it is not a MAC or an authority
//! source. Older writers must not write this format.

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

use crate::actions::{PlanAction, PlanEvent};
use crate::error::{PlanError, PlanResult};
use crate::ids::{PlanId, RevisionId};
use crate::parent_progress::ParentProgressReceipt;
use crate::types::Plan;

/// Hard limit including LF, bounding read and serialization buffers.
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    version: u32,
    plan_id: PlanId,
    base_revision: RevisionId,
    events: Vec<PlanEvent>,
    bootstrap: Option<Plan>,
}

// Separate schemas preserve the exact v1 canonical checksum representation.
// In v2 the receipt is mandatory, non-null and covered by the frame digest.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgressPayload {
    version: u32,
    plan_id: PlanId,
    base_revision: RevisionId,
    events: Vec<PlanEvent>,
    receipt: ParentProgressReceipt,
}

#[derive(Serialize)]
struct BorrowedProgressPayload<'a> {
    version: u32,
    plan_id: &'a PlanId,
    base_revision: RevisionId,
    events: &'a [PlanEvent],
    receipt: &'a ParentProgressReceipt,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame<T = Payload> {
    batch: T,
    digest: String,
}

// Same field order as Payload: serialization without unbounded copies of
// caller-supplied actions or the initial Create state.
#[derive(Serialize)]
struct BorrowedPayload<'a> {
    version: u32,
    plan_id: &'a PlanId,
    base_revision: RevisionId,
    events: &'a [PlanEvent],
    bootstrap: Option<&'a Plan>,
}

/// Validated stream segment; offsets are relative to the reader start.
pub(crate) struct WalRead {
    pub(crate) events: Vec<PlanEvent>,
    pub(crate) receipts: Vec<ParentProgressReceipt>,
    pub(crate) bootstrap: Option<Plan>,
    pub(crate) valid_len: u64,
    pub(crate) torn_tail: bool,
    pub(crate) last_revision: RevisionId,
    pub(crate) committed_revisions: Vec<RevisionId>,
}

fn invalid(message: &'static str) -> PlanError {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message).into()
}

#[derive(Default)]
struct BoundedBytes(Vec<u8>);

impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        // Reserve one byte for the record terminator.
        if bytes.len() > (MAX_RECORD_BYTES - 1).saturating_sub(self.0.len()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Plan-WAL record exceeds 16 MiB",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn serialized(value: &impl Serialize) -> PlanResult<Vec<u8>> {
    let mut bytes = BoundedBytes::default();
    serde_json::to_writer(&mut bytes, value)?;
    Ok(bytes.0)
}

fn digest(payload: &impl Serialize) -> PlanResult<String> {
    digest_with_domain(payload, b"harw-plan-wal:v1\0")
}

fn digest_with_domain(payload: &impl Serialize, domain: &[u8]) -> PlanResult<String> {
    // A fixed domain separator prevents confusion with other digest formats.
    let bytes = serialized(payload)?;
    let mut material = BoundedBytes::default();
    material.write_all(domain)?;
    material.write_all(&bytes)?;
    Ok(harw_types::ContentDigest::of(&material.0).to_string())
}

fn validate_events(
    events: &[PlanEvent],
    plan_id: &PlanId,
    base: RevisionId,
) -> PlanResult<RevisionId> {
    if events.is_empty() {
        return Err(invalid("Empty Plan-WAL transaction"));
    }
    let mut revision = base.value();
    for event in events {
        revision = revision
            .checked_add(1)
            .ok_or_else(|| invalid("Plan-WAL revision overflow"))?;
        if event.revision.value() != revision {
            return Err(invalid("Plan-WAL revision gap or duplicate"));
        }
        if let PlanAction::Create {
            plan_id: created, ..
        } = &event.action
        {
            if revision != 1 || created != plan_id {
                return Err(invalid("Invalid Plan-WAL Create identity or revision"));
            }
        } else if revision == 1 {
            return Err(invalid("Plan-WAL must begin with Create"));
        }
    }
    Ok(RevisionId::new(revision))
}

fn validate_bootstrap(payload: &Payload) -> PlanResult<()> {
    if payload.base_revision.value() != 0 {
        return if payload.bootstrap.is_none() {
            Ok(())
        } else {
            Err(invalid("Unexpected Plan-WAL bootstrap after Create"))
        };
    }
    let Some(plan) = &payload.bootstrap else {
        return Err(invalid("Missing Plan-WAL bootstrap"));
    };
    let Some(event) = payload.events.first() else {
        return Err(invalid("Missing Plan-WAL Create"));
    };
    validate_create(plan, event, &payload.plan_id, payload.events.len())
}

fn validate_create(
    plan: &Plan,
    event: &PlanEvent,
    expected_id: &PlanId,
    event_count: usize,
) -> PlanResult<()> {
    let PlanAction::Create { plan_id, goal } = &event.action else {
        return Err(invalid("Bootstrap does not describe Create"));
    };
    if event_count != 1
        || plan.id != *plan_id
        || plan.id != *expected_id
        || plan.revision.value() != 1
        || plan.parent_revision.is_some()
        || plan.goal_statement != *goal
        || plan.goal_id.is_some()
        || !plan.nodes.is_empty()
        || plan.created_at != event.applied_at
        || plan.updated_at != event.applied_at
    {
        return Err(invalid("Plan-WAL bootstrap disagrees with Create"));
    }
    Ok(())
}

/// Serializes a complete batch as one validated, bounded commit record.
pub(crate) fn encode(candidate: &Plan, events: &[PlanEvent]) -> PlanResult<Vec<u8>> {
    let first = events
        .first()
        .ok_or_else(|| invalid("Empty Plan-WAL transaction"))?;
    let base = first
        .revision
        .value()
        .checked_sub(1)
        .ok_or_else(|| invalid("Zero Plan-WAL event revision"))?;
    let base_revision = RevisionId::new(base);
    if validate_events(events, &candidate.id, base_revision)? != candidate.revision {
        return Err(invalid("Plan-WAL candidate revision disagrees with batch"));
    }
    if base == 0 {
        validate_create(candidate, first, &candidate.id, events.len())?;
    }
    let payload = BorrowedPayload {
        version: VERSION,
        plan_id: &candidate.id,
        base_revision,
        events,
        bootstrap: (base == 0).then_some(candidate),
    };
    let frame = Frame {
        digest: digest(&payload)?,
        batch: payload,
    };
    let mut bytes = serialized(&frame)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Serializes a restricted progress transaction with receipt provenance.
/// The store validates transition semantics; this layer binds frame identity
/// and boundaries and ensures no receipt can outlive its transaction.
pub(crate) fn encode_progress(
    candidate: &Plan,
    events: &[PlanEvent],
    receipt: &ParentProgressReceipt,
) -> PlanResult<Vec<u8>> {
    let base = receipt.before_revision;
    if base.value() == 0
        || receipt.schema_version != 1
        || receipt.plan_id != candidate.id
        || receipt.after_revision != candidate.revision
        || validate_events(events, &candidate.id, base)? != candidate.revision
    {
        return Err(invalid("Progress receipt disagrees with WAL transaction"));
    }
    let payload = BorrowedProgressPayload {
        version: 2,
        plan_id: &candidate.id,
        base_revision: base,
        events,
        receipt,
    };
    let frame = Frame {
        digest: digest_with_domain(&payload, b"harw-plan-wal:v2\0")?,
        batch: payload,
    };
    let mut bytes = serialized(&frame)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Reads only complete transactions. An incomplete EOF suffix is excluded;
/// the caller may truncate it only while holding the writer lock.
/// `base_revision` is the last validated history revision, not the snapshot
/// revision. Events already covered by a checkpoint are still validated.
pub(crate) fn read(
    mut reader: impl BufRead,
    plan_id: &PlanId,
    base_revision: RevisionId,
) -> PlanResult<WalRead> {
    let mut result = WalRead {
        events: Vec::new(),
        receipts: Vec::new(),
        bootstrap: None,
        valid_len: 0,
        torn_tail: false,
        last_revision: base_revision,
        committed_revisions: Vec::new(),
    };
    loop {
        let mut line = Vec::new();
        loop {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                result.torn_tail = !line.is_empty();
                return Ok(result);
            }
            let take = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |position| position + 1);
            if take > MAX_RECORD_BYTES.saturating_sub(line.len()) {
                return Err(invalid("Plan-WAL record exceeds 16 MiB"));
            }
            line.extend_from_slice(&available[..take]);
            reader.consume(take);
            if line.last() == Some(&b'\n') {
                break;
            }
        }
        if std::str::from_utf8(&line).is_ok_and(|text| text.trim().is_empty()) {
            result.valid_len = result
                .valid_len
                .checked_add(line.len() as u64)
                .ok_or_else(|| invalid("Plan-WAL byte offset overflow"))?;
            continue;
        }
        let value: serde_json::Value = serde_json::from_slice(&line)?;
        if value.get("batch").is_some() {
            match value
                .get("batch")
                .and_then(|batch| batch.get("version"))
                .and_then(serde_json::Value::as_u64)
            {
                Some(1) => {
                    let frame: Frame = serde_json::from_slice(&line)?;
                    if frame.batch.version != VERSION
                        || frame.batch.plan_id != *plan_id
                        || frame.batch.base_revision != result.last_revision
                        || frame.digest != digest(&frame.batch)?
                    {
                        return Err(invalid(
                            "Invalid Plan-WAL frame identity, version, base or digest",
                        ));
                    }
                    let last = validate_events(&frame.batch.events, plan_id, result.last_revision)?;
                    validate_bootstrap(&frame.batch)?;
                    result.last_revision = last;
                    if let Some(bootstrap) = frame.batch.bootstrap {
                        result.bootstrap = Some(bootstrap);
                    }
                    result.events.extend(frame.batch.events);
                }
                Some(2) => {
                    // Deserialize directly from bytes, not Value: duplicate
                    // fields must be rejected rather than silently normalized.
                    let frame: Frame<ProgressPayload> = serde_json::from_slice(&line)?;
                    let payload = frame.batch;
                    if payload.version != 2
                        || payload.plan_id != *plan_id
                        || payload.base_revision != result.last_revision
                        || payload.base_revision.value() == 0
                        || frame.digest != digest_with_domain(&payload, b"harw-plan-wal:v2\0")?
                    {
                        return Err(invalid("Invalid progress WAL identity, base or digest"));
                    }
                    let last = validate_events(&payload.events, plan_id, result.last_revision)?;
                    if payload.receipt.schema_version != 1
                        || payload.receipt.plan_id != *plan_id
                        || payload.receipt.before_revision != payload.base_revision
                        || payload.receipt.after_revision != last
                    {
                        return Err(invalid("Progress receipt disagrees with WAL transaction"));
                    }
                    result.last_revision = last;
                    result.events.extend(payload.events);
                    result.receipts.push(payload.receipt);
                }
                _ => return Err(invalid("Unsupported Plan-WAL frame version")),
            }
        } else {
            // Legacy history intentionally contains one event per line.
            let event: PlanEvent = serde_json::from_slice(&line)?;
            result.last_revision =
                validate_events(std::slice::from_ref(&event), plan_id, result.last_revision)?;
            result.events.push(event);
        }
        result.committed_revisions.push(result.last_revision);
        result.valid_len = result
            .valid_len
            .checked_add(line.len() as u64)
            .ok_or_else(|| invalid("Plan-WAL byte offset overflow"))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use time::OffsetDateTime;

    fn created() -> (Plan, PlanEvent) {
        let now = OffsetDateTime::UNIX_EPOCH;
        let plan = Plan {
            id: PlanId::new("wal-test"),
            revision: RevisionId::new(1),
            parent_revision: None,
            goal_statement: "goal".into(),
            goal_id: None,
            nodes: Vec::new(),
            created_at: now,
            updated_at: now,
            tenant: None,
        };
        let event = PlanEvent {
            revision: plan.revision,
            action: PlanAction::Create {
                plan_id: plan.id.clone(),
                goal: plan.goal_statement.clone(),
            },
            actor: "runtime".into(),
            applied_at: now,
        };
        (plan, event)
    }

    fn later(revision: u64) -> PlanEvent {
        PlanEvent {
            revision: RevisionId::new(revision),
            action: PlanAction::Inspect,
            actor: "runtime".into(),
            applied_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn every_batch_prefix_is_uncommitted_until_terminator() -> TestResult {
        let (mut plan, _) = created();
        plan.revision = RevisionId::new(3);
        let bytes = encode(&plan, &[later(2), later(3)])?;
        for length in 0..bytes.len() {
            let parsed = read(&bytes[..length], &plan.id, RevisionId::new(1))?;
            assert!(parsed.events.is_empty(), "prefix {length}");
            assert_eq!(parsed.valid_len, 0);
            assert_eq!(parsed.torn_tail, length != 0);
        }
        let parsed = read(bytes.as_slice(), &plan.id, RevisionId::new(1))?;
        assert_eq!(parsed.events.len(), 2);
        assert_eq!(parsed.valid_len, bytes.len() as u64);
        assert!(!parsed.torn_tail);
        Ok(())
    }

    #[test]
    fn every_create_prefix_has_no_bootstrap_until_terminator() -> TestResult {
        let (plan, event) = created();
        let bytes = encode(&plan, &[event])?;
        for length in 0..bytes.len() {
            let parsed = read(&bytes[..length], &plan.id, RevisionId::new(0))?;
            assert!(parsed.bootstrap.is_none(), "prefix {length}");
            assert!(parsed.events.is_empty(), "prefix {length}");
            assert_eq!(parsed.last_revision, RevisionId::new(0));
            assert_eq!(parsed.valid_len, 0);
        }
        assert!(
            read(bytes.as_slice(), &plan.id, RevisionId::new(0))?
                .bootstrap
                .is_some()
        );
        Ok(())
    }

    fn progress_receipt(plan: &Plan) -> ParentProgressReceipt {
        let digest = harw_types::ContentDigest::of(b"fixture");
        ParentProgressReceipt {
            schema_version: 1,
            admission: crate::parent_progress::ParentAdmission {
                parent_work_id: "parent-fixture".into(),
                plan_id: plan.id.clone(),
                revision: RevisionId::new(1),
                snapshot_digest: digest,
            },
            plan_id: plan.id.clone(),
            node_id: crate::ids::TaskId::new("collect"),
            attempt: 1,
            required_sources: vec!["docs/readme.md".into()],
            contract_digest: digest,
            artifact_digest: digest,
            publishing_epoch: 1,
            operation_id: "collection-1".into(),
            before_revision: RevisionId::new(1),
            after_revision: plan.revision,
            before_digest: digest,
            after_digest: digest,
            previous_chain_head: None,
        }
    }

    #[test]
    fn progress_frame_is_atomic_with_its_receipt_at_every_prefix() -> TestResult {
        let (mut plan, _) = created();
        plan.revision = RevisionId::new(3);
        let receipt = progress_receipt(&plan);
        let bytes = encode_progress(&plan, &[later(2), later(3)], &receipt)?;
        for length in 0..bytes.len() {
            let parsed = read(&bytes[..length], &plan.id, RevisionId::new(1))?;
            assert!(parsed.events.is_empty(), "events at prefix {length}");
            assert!(parsed.receipts.is_empty(), "receipt at prefix {length}");
            assert_eq!(parsed.valid_len, 0);
        }
        let parsed = read(bytes.as_slice(), &plan.id, RevisionId::new(1))?;
        assert_eq!(parsed.events.len(), 2);
        assert_eq!(parsed.receipts, vec![receipt]);
        assert_eq!(parsed.committed_revisions, vec![plan.revision]);
        assert_eq!(parsed.valid_len, bytes.len() as u64);
        Ok(())
    }

    #[test]
    fn progress_receipt_is_checksummed_and_bound_to_transaction() -> TestResult {
        let (mut plan, _) = created();
        plan.revision = RevisionId::new(3);
        let receipt = progress_receipt(&plan);
        let bytes = encode_progress(&plan, &[later(2), later(3)], &receipt)?;
        let mut frame: Frame<ProgressPayload> = serde_json::from_slice(&bytes)?;
        frame.batch.receipt.operation_id = "tampered".into();
        let mut tampered = serde_json::to_vec(&frame)?;
        tampered.push(b'\n');
        assert!(read(tampered.as_slice(), &plan.id, RevisionId::new(1)).is_err());
        frame.batch.receipt.after_revision = RevisionId::new(4);
        frame.digest = digest_with_domain(&frame.batch, b"harw-plan-wal:v2\0")?;
        let mut inconsistent = serde_json::to_vec(&frame)?;
        inconsistent.push(b'\n');
        assert!(read(inconsistent.as_slice(), &plan.id, RevisionId::new(1)).is_err());
        assert!(encode_progress(&plan, &[later(2), later(3)], &frame.batch.receipt).is_err());
        Ok(())
    }

    #[test]
    fn progress_metadata_rejects_null_missing_duplicate_and_unknown_fields() -> TestResult {
        let (mut plan, _) = created();
        plan.revision = RevisionId::new(3);
        let receipt = progress_receipt(&plan);
        let bytes = encode_progress(&plan, &[later(2), later(3)], &receipt)?;
        let original: serde_json::Value = serde_json::from_slice(&bytes)?;
        for field in ["missing", "null", "unknown", "version"] {
            let mut value = original.clone();
            match field {
                "missing" => {
                    value["batch"]
                        .as_object_mut()
                        .ok_or(TestError::Missing("batch"))?
                        .remove("receipt");
                }
                "null" => value["batch"]["receipt"] = serde_json::Value::Null,
                "unknown" => value["batch"]["receipt"]["authority"] = serde_json::json!(true),
                _ => value["batch"]["version"] = serde_json::json!(1),
            }
            let mut malformed = serde_json::to_vec(&value)?;
            malformed.push(b'\n');
            if field != "version" {
                assert!(serde_json::from_slice::<Frame<ProgressPayload>>(&malformed).is_err());
            }
            assert!(read(malformed.as_slice(), &plan.id, RevisionId::new(1)).is_err());
        }
        let text =
            String::from_utf8(bytes).map_err(|error| TestError::Unexpected(error.to_string()))?;
        let duplicate = text.replacen("\"receipt\":{", "\"receipt\":null,\"receipt\":{", 1);
        assert!(read(duplicate.as_bytes(), &plan.id, RevisionId::new(1)).is_err());
        Ok(())
    }

    #[test]
    fn mixed_legacy_v1_and_v2_stream_retains_progress_metadata() -> TestResult {
        let (mut plan, event) = created();
        let mut bytes = serde_json::to_vec(&event)?;
        bytes.push(b'\n');
        plan.revision = RevisionId::new(2);
        bytes.extend(encode(&plan, &[later(2)])?);
        plan.revision = RevisionId::new(4);
        let mut receipt = progress_receipt(&plan);
        receipt.before_revision = RevisionId::new(2);
        bytes.extend(encode_progress(&plan, &[later(3), later(4)], &receipt)?);
        let parsed = read(bytes.as_slice(), &plan.id, RevisionId::new(0))?;
        assert_eq!(parsed.events.len(), 4);
        assert_eq!(parsed.receipts, vec![receipt]);
        Ok(())
    }

    #[test]
    fn v1_rejects_progress_metadata_and_v2_requires_it() -> TestResult {
        let (plan, event) = created();
        let bytes = encode(&plan, &[event])?;
        let original: serde_json::Value = serde_json::from_slice(&bytes)?;
        for version in [1, 2, 3] {
            for receipt in [
                None,
                Some(serde_json::Value::Null),
                Some(serde_json::json!({})),
            ] {
                // Unmodified v1 is the compatibility control.
                if version == 1 && receipt.is_none() {
                    continue;
                }
                let mut value = original.clone();
                value["batch"]["version"] = serde_json::json!(version);
                if let Some(receipt) = receipt {
                    value["batch"]["receipt"] = receipt;
                }
                let mut mutated = serde_json::to_vec(&value)?;
                mutated.push(b'\n');
                assert!(read(mutated.as_slice(), &plan.id, RevisionId::new(0)).is_err());
            }
        }
        Ok(())
    }

    #[test]
    fn duplicate_json_fields_are_rejected() -> TestResult {
        let (plan, event) = created();
        let bytes = encode(&plan, &[event])?;
        let mut duplicated = b"{\"digest\":\"ignored\",".to_vec();
        duplicated.extend_from_slice(&bytes[1..]);
        assert!(read(duplicated.as_slice(), &plan.id, RevisionId::new(0)).is_err());
        Ok(())
    }

    #[test]
    fn revision_overflow_is_an_error_not_a_panic() -> TestResult {
        let (mut plan, _) = created();
        plan.revision = RevisionId::new(u64::MAX);
        let bytes = encode(&plan, &[later(u64::MAX)])?;
        let accepted = read(bytes.as_slice(), &plan.id, RevisionId::new(u64::MAX - 1))?;
        assert_eq!(accepted.last_revision, RevisionId::new(u64::MAX));
        // A stream claiming an additional event cannot wrap its revision.
        let mut legacy = serde_json::to_vec(&later(0))?;
        legacy.push(b'\n');
        assert!(read(legacy.as_slice(), &plan.id, RevisionId::new(u64::MAX)).is_err());
        Ok(())
    }

    #[test]
    fn long_legacy_stream_followed_by_new_batch_preserves_all_events() -> TestResult {
        let (mut plan, event) = created();
        let mut bytes = serde_json::to_vec(&event)?;
        bytes.push(b'\n');
        for revision in 2..=2048 {
            serde_json::to_writer(&mut bytes, &later(revision))?;
            bytes.push(b'\n');
        }
        plan.revision = RevisionId::new(2050);
        bytes.extend_from_slice(&encode(&plan, &[later(2049), later(2050)])?);
        let parsed = read(bytes.as_slice(), &plan.id, RevisionId::new(0))?;
        assert_eq!(parsed.events.len(), 2050);
        assert_eq!(parsed.committed_revisions.len(), 2049);
        assert_eq!(parsed.last_revision, RevisionId::new(2050));
        assert_eq!(parsed.valid_len, bytes.len() as u64);
        Ok(())
    }

    #[test]
    fn bootstrap_roundtrips_exact_state() -> TestResult {
        let (mut plan, event) = created();
        plan.tenant = Some(harw_types::TenantId::from_str("tenant-wal"));
        let bytes = encode(&plan, &[event])?;
        let parsed = read(bytes.as_slice(), &plan.id, RevisionId::new(0))?;
        let recovered = parsed.bootstrap.ok_or(TestError::Missing("bootstrap"))?;
        assert_eq!(
            serde_json::to_value(&recovered)?,
            serde_json::to_value(&plan)?
        );
        Ok(())
    }

    #[test]
    fn legacy_then_frame_reads_and_rejects_duplicate_or_gap() -> TestResult {
        let (mut plan, event) = created();
        let mut bytes = serde_json::to_vec(&event)?;
        bytes.push(b'\n');
        plan.revision = RevisionId::new(3);
        let frame = encode(&plan, &[later(2), later(3)])?;
        bytes.extend_from_slice(&frame);
        assert_eq!(
            read(bytes.as_slice(), &plan.id, RevisionId::new(0))?
                .events
                .len(),
            3
        );
        bytes.extend_from_slice(&frame);
        assert!(read(bytes.as_slice(), &plan.id, RevisionId::new(0)).is_err());
        assert!(read(frame.as_slice(), &plan.id, RevisionId::new(0)).is_err());
        Ok(())
    }

    #[test]
    fn small_reader_buffers_preserve_boundaries_and_legacy_blank_lines() -> TestResult {
        let (plan, event) = created();
        let mut bytes = b" \n\t\r\n".to_vec();
        bytes.extend_from_slice(&encode(&plan, &[event])?);
        bytes.extend_from_slice(b"{torn");
        let reader = std::io::BufReader::with_capacity(1, bytes.as_slice());
        let parsed = read(reader, &plan.id, RevisionId::new(0))?;
        assert_eq!(parsed.events.len(), 1);
        assert_eq!(parsed.committed_revisions, vec![RevisionId::new(1)]);
        assert_eq!(parsed.valid_len, (bytes.len() - 5) as u64);
        assert!(parsed.torn_tail);
        Ok(())
    }

    #[test]
    fn checksummed_but_inconsistent_bootstrap_is_rejected() -> TestResult {
        let (plan, event) = created();
        let bytes = encode(&plan, &[event])?;
        let mut frame: Frame = serde_json::from_slice(&bytes)?;
        let bootstrap = frame
            .batch
            .bootstrap
            .as_mut()
            .ok_or(TestError::Missing("bootstrap"))?;
        bootstrap.goal_statement = "wrong goal".into();
        frame.digest = digest(&frame.batch)?;
        let mut bytes = serde_json::to_vec(&frame)?;
        bytes.push(b'\n');
        assert!(read(bytes.as_slice(), &plan.id, RevisionId::new(0)).is_err());
        Ok(())
    }

    #[test]
    fn inconsistent_bootstrap_and_noncontiguous_batch_are_rejected() -> TestResult {
        let (mut plan, event) = created();
        plan.goal_statement = "different".into();
        assert!(encode(&plan, &[event]).is_err());
        plan.revision = RevisionId::new(4);
        assert!(encode(&plan, &[later(2), later(4)]).is_err());
        Ok(())
    }

    #[test]
    fn serialization_and_streaming_read_are_bounded() -> TestResult {
        let (mut plan, mut event) = created();
        plan.goal_statement = "x".repeat(MAX_RECORD_BYTES);
        event.action = PlanAction::Create {
            plan_id: plan.id.clone(),
            goal: plan.goal_statement.clone(),
        };
        assert!(encode(&plan, &[event]).is_err());
        let oversized = vec![b'x'; MAX_RECORD_BYTES + 1];
        assert!(read(oversized.as_slice(), &plan.id, RevisionId::new(0)).is_err());
        Ok(())
    }

    #[test]
    fn complete_corruption_and_foreign_plan_fail_closed() -> TestResult {
        let (plan, event) = created();
        let bytes = encode(&plan, &[event])?;
        let mut value: serde_json::Value = serde_json::from_slice(&bytes)?;
        value["digest"] = serde_json::Value::String("bad".into());
        let mut corrupted = serde_json::to_vec(&value)?;
        corrupted.push(b'\n');
        assert!(read(corrupted.as_slice(), &plan.id, RevisionId::new(0)).is_err());
        assert!(
            read(
                bytes.as_slice(),
                &PlanId::new("foreign"),
                RevisionId::new(0)
            )
            .is_err()
        );
        assert!(read(&b"{broken}\n"[..], &plan.id, RevisionId::new(0)).is_err());
        Ok(())
    }
}

use super::*;
use harw_types::cancel::CancelReason;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{Semaphore, mpsc};

fn jpeg() -> Arc<[u8]> {
    Arc::from([0xFF, 0xD8, 0xFF, 0xE0, 0xFF, 0xD9])
}

#[test]
fn rejects_out_of_bounds_batches_and_duplicate_ids() {
    assert!(validate_description_batch(&[]).is_err());
    let mut items: Vec<_> = (0..50).map(|id| (id, jpeg())).collect();
    assert!(validate_description_batch(&items).is_ok());
    items.push((50, jpeg()));
    assert!(validate_description_batch(&items).is_err());
    assert!(validate_description_batch(&[(7, jpeg()), (7, jpeg())]).is_err());
}

#[test]
fn converts_only_visible_captions_and_keeps_cutoff_reason() {
    assert_eq!(
        description_outcome(FinishReason::EndOfGeneration, "  Caption  ".into()),
        LocalImageDescriptionOutcome::Caption {
            text: "Caption".into()
        },
    );
    assert_eq!(
        description_outcome(FinishReason::MaxTokens, "part".into()),
        LocalImageDescriptionOutcome::OutputLimit {
            text: "part".into()
        },
    );
    assert_eq!(
        description_outcome(FinishReason::ContextFull, "part".into()),
        LocalImageDescriptionOutcome::ContextLimit {
            text: "part".into()
        },
    );
    assert_eq!(
        description_outcome(FinishReason::EndOfGeneration, " ".into()),
        LocalImageDescriptionOutcome::Failed,
    );
}

#[tokio::test]
async fn batch_has_one_device_permit_and_reports_stable_ids_as_workers_complete() {
    let control = LocalInferenceControl::default();
    assert!(control.set_image_concurrency(3));
    let cancel = CancelToken::new();
    let (begun, mut running) = mpsc::unbounded_channel();
    let (report, mut reports) = mpsc::unbounded_channel();
    let releases: HashMap<_, _> = [10, 40, 80, 100]
        .into_iter()
        .map(|id| (id, Arc::new(Semaphore::new(0))))
        .collect();
    let workers = releases.clone();
    let worker_control = control.clone();
    let batch = tokio::spawn(async move {
        run_description_batch(
            &worker_control,
            vec![
                (2, Arc::from([0x89])),
                (10, jpeg()),
                (40, jpeg()),
                (80, jpeg()),
                (100, jpeg()),
            ],
            &cancel,
            || 3,
            move |id, _, _| {
                let gate = Arc::clone(&workers[&id]);
                let begun = begun.clone();
                async move {
                    begun.send(id).expect("worker started");
                    gate.acquire().await.expect("worker release").forget();
                    LocalImageDescriptionOutcome::Caption {
                        text: id.to_string(),
                    }
                }
            },
            &move |result| {
                report.send(result).expect("callback delivered");
            },
        )
        .await
    });
    assert_eq!(
        reports.recv().await.expect("invalid first").outcome,
        LocalImageDescriptionOutcome::InvalidInput
    );
    let mut first = vec![];
    for _ in 0..3 {
        first.push(running.recv().await.expect("first wave"));
    }
    first.sort_unstable();
    assert_eq!(first, [10, 40, 80]);
    assert!(running.try_recv().is_err());

    let other_control = control.clone();
    let waiting = tokio::spawn(async move { other_control.begin(None).await });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());

    releases[&80].add_permits(1);
    assert_eq!(reports.recv().await.expect("first result").id, 80);
    assert_eq!(running.recv().await, Some(100));
    for id in [100, 40, 10] {
        releases[&id].add_permits(1);
        let result = reports.recv().await.expect("caption");
        assert_eq!(result.id, id);
        assert_eq!(
            result.outcome,
            LocalImageDescriptionOutcome::Caption {
                text: id.to_string()
            }
        );
    }
    assert!(batch.await.expect("outer join").is_ok());
    assert!(waiting.await.expect("next admission").is_some());
}

#[tokio::test]
async fn cancellation_never_admits_a_later_image_or_reports_its_caption() {
    let control = LocalInferenceControl::default();
    let cancel = CancelToken::new();
    let active = cancel.clone();
    let (begun, mut running) = mpsc::unbounded_channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let call_count = Arc::clone(&calls);
    let batch = tokio::spawn(async move {
        run_description_batch(
            &control,
            vec![(1, jpeg()), (2, jpeg())],
            &active,
            || 5,
            move |id, _, _| {
                call_count.fetch_add(1, Ordering::Relaxed);
                begun.send(id).expect("worker started");
                std::future::pending()
            },
            &|_| panic!("cancelled batch reported a result"),
        )
        .await
    });
    assert_eq!(running.recv().await, Some(1));
    cancel.cancel(CancelReason::User);
    assert!(matches!(
        batch.await.expect("outer join"),
        Err(ModelError::Cancelled)
    ));
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn per_image_failure_does_not_abort_other_approved_images() {
    let control = LocalInferenceControl::default();
    let results = Mutex::new(Vec::new());
    run_description_batch(
        &control,
        vec![(1, jpeg()), (2, jpeg())],
        &CancelToken::new(),
        || 5,
        |id, _, _| async move {
            if id == 1 {
                LocalImageDescriptionOutcome::Failed
            } else {
                LocalImageDescriptionOutcome::Caption {
                    text: "other image".into(),
                }
            }
        },
        &|result| results.lock().expect("result mutex").push(result),
    )
    .await
    .expect("batch continues");
    let results = results.into_inner().expect("result mutex");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].outcome, LocalImageDescriptionOutcome::Failed);
    assert_eq!(
        results[1].outcome,
        LocalImageDescriptionOutcome::Caption {
            text: "other image".into()
        }
    );
}

#[test]
fn android_worker_cap_distinguishes_known_headroom_from_preference() {
    const GIB: u64 = 1024 * 1024 * 1024;
    assert_eq!(android_vision_worker_cap(Some(3 * GIB), Some(9 * GIB), 8), 5);
    assert_eq!(android_vision_worker_cap(Some(3 * GIB), Some(8 * GIB), 8), 4);
    assert_eq!(android_vision_worker_cap(Some(3 * GIB), Some(8 * GIB), 5), 2);
    assert_eq!(android_vision_worker_cap(Some(4 * GIB), Some(4 * GIB), 8), 1);
    assert_eq!(android_vision_worker_cap(Some(3 * GIB), None, 8), 1);
}

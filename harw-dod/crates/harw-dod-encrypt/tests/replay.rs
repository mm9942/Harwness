//! Replay window: fresh, duplicate, too old, reorder within the window,
//! window slide, check-before-commit and the per-sender bound.

mod common;

use common::{TestResult, ctx};
use harw_dod_encrypt::{
    EncryptError, NodeId, REPLAY_WINDOW_BITS, ReplayWindow, SenderReplayWindows,
};

#[test]
fn test_first_sequence_is_accepted_and_duplicate_rejected() -> TestResult {
    let mut w = ReplayWindow::new();
    w.commit(10).map_err(ctx("first"))?;
    assert_eq!(w.highest(), Some(10));
    assert_eq!(w.commit(10).err(), Some(EncryptError::ReplayDuplicate));
    Ok(())
}

#[test]
fn test_sequence_zero_is_a_normal_number() -> TestResult {
    let mut w = ReplayWindow::new();
    w.commit(0).map_err(ctx("zero"))?;
    assert_eq!(w.commit(0).err(), Some(EncryptError::ReplayDuplicate));
    w.commit(1).map_err(ctx("one"))?;
    Ok(())
}

#[test]
fn test_reorder_within_window_is_accepted_once() -> TestResult {
    let mut w = ReplayWindow::new();
    for seq in [5, 3, 4, 1, 2] {
        w.commit(seq).map_err(ctx("in-window reorder"))?;
    }
    assert_eq!(w.highest(), Some(5));
    for seq in 1..=5 {
        assert_eq!(
            w.commit(seq).err(),
            Some(EncryptError::ReplayDuplicate),
            "seq {seq}"
        );
    }
    Ok(())
}

#[test]
fn test_window_edge_is_exact() -> TestResult {
    let top = 1_000u64;
    let mut w = ReplayWindow::new();
    w.commit(top).map_err(ctx("top"))?;

    // The oldest number still inside the window is accepted once.
    let oldest_inside = top - (REPLAY_WINDOW_BITS - 1);
    w.commit(oldest_inside).map_err(ctx("oldest inside"))?;
    assert_eq!(
        w.commit(oldest_inside).err(),
        Some(EncryptError::ReplayDuplicate)
    );

    // One below is too old, even though it was never seen.
    assert_eq!(
        w.commit(top - REPLAY_WINDOW_BITS).err(),
        Some(EncryptError::ReplayTooOld)
    );
    Ok(())
}

#[test]
fn test_slide_forward_keeps_marks_that_stay_inside() -> TestResult {
    let mut w = ReplayWindow::new();
    w.commit(100).map_err(ctx("100"))?;
    w.commit(98).map_err(ctx("98"))?;
    w.commit(110).map_err(ctx("110"))?;
    assert_eq!(w.commit(100).err(), Some(EncryptError::ReplayDuplicate));
    assert_eq!(w.commit(98).err(), Some(EncryptError::ReplayDuplicate));
    w.commit(99).map_err(ctx("99 never seen"))?;
    Ok(())
}

#[test]
fn test_large_jump_clears_the_window() -> TestResult {
    let mut w = ReplayWindow::new();
    w.commit(1).map_err(ctx("1"))?;
    let far = 1 + 10 * REPLAY_WINDOW_BITS;
    w.commit(far).map_err(ctx("far"))?;
    assert_eq!(w.commit(1).err(), Some(EncryptError::ReplayTooOld));
    // Everything just below the new top is fresh.
    w.commit(far - 1).map_err(ctx("far - 1"))?;
    Ok(())
}

#[test]
fn test_jump_by_exactly_the_window_width() -> TestResult {
    let mut w = ReplayWindow::new();
    w.commit(0).map_err(ctx("0"))?;
    w.commit(REPLAY_WINDOW_BITS).map_err(ctx("width"))?;
    assert_eq!(w.commit(0).err(), Some(EncryptError::ReplayTooOld));
    w.commit(1).map_err(ctx("1 is inside"))?;
    Ok(())
}

#[test]
fn test_u64_max_is_handled() -> TestResult {
    let mut w = ReplayWindow::new();
    w.commit(u64::MAX).map_err(ctx("max"))?;
    assert_eq!(
        w.commit(u64::MAX).err(),
        Some(EncryptError::ReplayDuplicate)
    );
    assert_eq!(w.commit(0).err(), Some(EncryptError::ReplayTooOld));
    w.commit(u64::MAX - 1).map_err(ctx("max - 1"))?;
    Ok(())
}

#[test]
fn test_check_does_not_consume_state() -> TestResult {
    let mut w = ReplayWindow::new();
    w.check(7).map_err(ctx("check"))?;
    w.check(7).map_err(ctx("check again"))?;
    assert_eq!(w.highest(), None);
    w.commit(7).map_err(ctx("commit"))?;
    assert_eq!(w.check(7).err(), Some(EncryptError::ReplayDuplicate));
    Ok(())
}

#[test]
fn test_rejected_commit_leaves_state_unchanged() -> TestResult {
    let mut w = ReplayWindow::new();
    w.commit(500).map_err(ctx("500"))?;
    let before = w.clone();
    assert!(w.commit(500).is_err());
    assert!(w.commit(1).is_err());
    assert_eq!(w, before);
    Ok(())
}

#[test]
fn test_senders_have_independent_windows() -> TestResult {
    let a = NodeId::new("node-a").map_err(ctx("a"))?;
    let b = NodeId::new("node-b").map_err(ctx("b"))?;
    let mut windows = SenderReplayWindows::new(8);
    windows.commit(&a, 1).map_err(ctx("a1"))?;
    windows
        .commit(&b, 1)
        .map_err(ctx("b1: same number, other sender"))?;
    assert_eq!(
        windows.commit(&a, 1).err(),
        Some(EncryptError::ReplayDuplicate)
    );
    assert_eq!(windows.len(), 2);
    Ok(())
}

#[test]
fn test_sender_capacity_is_enforced_without_recording() -> TestResult {
    let a = NodeId::new("node-a").map_err(ctx("a"))?;
    let b = NodeId::new("node-b").map_err(ctx("b"))?;
    let mut windows = SenderReplayWindows::new(1);
    assert!(windows.is_empty());
    windows.commit(&a, 1).map_err(ctx("a1"))?;
    assert_eq!(
        windows.check(&b, 1).err(),
        Some(EncryptError::ReplayTooManySenders)
    );
    assert_eq!(
        windows.commit(&b, 1).err(),
        Some(EncryptError::ReplayTooManySenders)
    );
    assert_eq!(windows.len(), 1);
    // The known sender keeps working.
    windows.commit(&a, 2).map_err(ctx("a2"))?;
    Ok(())
}

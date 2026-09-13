//! Integrationstests der öffentlichen `harw-fsutil`-API gegen echte
//! Symlink-Fixtures in einem Tempdir.
//!
//! Die Fixtures bilden die Angriffsmuster nach, gegen die das Crate schützen
//! soll: Symlink als letztes Glied, Symlink als Zwischenglied, Ausbruch per
//! `..`/absolutem Pfad, Symlink-Schleifen und Symlinks auf fremde Dateien am
//! Ziel eines atomaren Schreibvorgangs.

use std::fs;
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use harw_fsutil::{
    AtomicWriteOptions, EntryType, OpenMode, WalkEntry, WalkLimits, WalkStop,
    ensure_private_regular, open_beneath, open_dir_nofollow, open_nofollow, walk_beneath,
    write_atomic,
};
use rustix::io::Errno;

/// Aufbau:
///
/// ```text
/// root/
///   data/secret.txt        "geheim"
///   data/loop -> .          Schleife
///   escape -> ../outside    zeigt aus der Wurzel heraus
///   hop -> data             Symlink als Zwischenglied
///   final -> data/secret.txt
///   dangling -> nirgends
/// outside/victim.txt       "fremd"
/// ```
struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("root");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(root.join("data")).expect("mkdir data");
    fs::create_dir_all(&outside).expect("mkdir outside");
    fs::write(root.join("data/secret.txt"), b"geheim").expect("write secret");
    fs::write(outside.join("victim.txt"), b"fremd").expect("write victim");
    symlink(".", root.join("data/loop")).expect("symlink loop");
    symlink("../outside", root.join("escape")).expect("symlink escape");
    symlink("data", root.join("hop")).expect("symlink hop");
    symlink("data/secret.txt", root.join("final")).expect("symlink final");
    symlink("nirgends", root.join("dangling")).expect("symlink dangling");
    Fixture {
        _tmp: tmp,
        root,
        outside,
    }
}

fn errno_in(err: &io::Error, candidates: &[Errno]) -> bool {
    candidates
        .iter()
        .any(|errno| err.raw_os_error() == Some(errno.raw_os_error()))
}

fn read_string(mut file: fs::File) -> String {
    let mut text = String::new();
    file.read_to_string(&mut text).expect("read");
    text
}

#[test]
fn letztes_glied_symlink_wird_nie_gefolgt() {
    let fx = fixture();
    let err = open_nofollow(&fx.root.join("final"), OpenMode::read_only()).unwrap_err();
    assert!(errno_in(&err, &[Errno::LOOP]), "{err:?}");

    let root = open_dir_nofollow(&fx.root).expect("root");
    let err = open_beneath(root.as_fd(), Path::new("final"), OpenMode::read_only()).unwrap_err();
    assert!(errno_in(&err, &[Errno::LOOP]), "{err:?}");
}

#[test]
fn zwischenglied_symlink_wird_bei_open_beneath_abgelehnt() {
    let fx = fixture();
    let root = open_dir_nofollow(&fx.root).expect("root");

    let secret = Path::new("data/secret.txt");
    let direct = open_beneath(root.as_fd(), secret, OpenMode::read_only()).expect("direkt");
    assert_eq!(read_string(direct), "geheim");

    for rel in ["hop/secret.txt", "escape/victim.txt", "data/loop/secret.txt"] {
        let err = open_beneath(root.as_fd(), Path::new(rel), OpenMode::read_only()).unwrap_err();
        assert!(errno_in(&err, &[Errno::LOOP, Errno::NOTDIR]), "{rel}: {err:?}");
    }
}

#[test]
fn ausbruch_per_punktpunkt_oder_absolut_wird_abgelehnt() {
    let fx = fixture();
    let root = open_dir_nofollow(&fx.root).expect("root");
    let absolute = fx.outside.join("victim.txt");
    let candidates = [
        Path::new("../outside/victim.txt"),
        Path::new("data/../../outside/victim.txt"),
        absolute.as_path(),
    ];
    for rel in candidates {
        let err = open_beneath(root.as_fd(), rel, OpenMode::read_only()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{rel:?}");
    }
}

#[test]
fn wurzel_als_symlink_wird_abgelehnt() {
    let fx = fixture();
    let err = open_dir_nofollow(&fx.root.join("hop")).unwrap_err();
    assert!(errno_in(&err, &[Errno::LOOP, Errno::NOTDIR]), "{err:?}");
    walk_beneath(
        &fx.root.join("hop"),
        WalkLimits {
            max_depth: 4,
            max_entries: 100,
            deadline: None,
        },
    )
    .unwrap_err();
}

#[test]
fn walk_folgt_keinen_symlinks_und_terminiert() {
    let fx = fixture();
    let limits = WalkLimits {
        max_depth: usize::MAX,
        max_entries: 1_000,
        deadline: None,
    };
    let mut walk = walk_beneath(&fx.root, limits).expect("walk");
    let entries: Vec<WalkEntry> = walk.by_ref().map(|entry| entry.expect("entry")).collect();
    let summary: Vec<(&str, EntryType)> = entries
        .iter()
        .map(|entry| (entry.rel_path.to_str().expect("utf8"), entry.entry_type))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("dangling", EntryType::Symlink),
            ("data", EntryType::Dir),
            ("data/loop", EntryType::Symlink),
            ("data/secret.txt", EntryType::File),
            ("escape", EntryType::Symlink),
            ("final", EntryType::Symlink),
            ("hop", EntryType::Symlink),
        ]
    );
    // Keine Einträge aus `outside/` (über `escape`) und keine Wiederholung über `data/loop`.
    assert_eq!(walk.stopped(), None);
}

#[test]
fn walk_grenzen_melden_stopped() {
    let fx = fixture();
    let depth = WalkLimits {
        max_depth: 1,
        max_entries: 1_000,
        deadline: None,
    };
    let mut walk = walk_beneath(&fx.root, depth).expect("walk");
    assert_eq!(walk.by_ref().count(), 5);
    assert_eq!(walk.stopped(), Some(WalkStop::DepthLimit));

    let entries = WalkLimits {
        max_depth: usize::MAX,
        max_entries: 2,
        deadline: None,
    };
    let mut walk = walk_beneath(&fx.root, entries).expect("walk");
    assert_eq!(walk.by_ref().count(), 2);
    assert_eq!(walk.stopped(), Some(WalkStop::EntryLimit));

    let deadline = WalkLimits {
        max_depth: usize::MAX,
        max_entries: 1_000,
        deadline: Some(std::time::Instant::now()),
    };
    let mut walk = walk_beneath(&fx.root, deadline).expect("walk");
    assert_eq!(walk.by_ref().count(), 0);
    assert_eq!(walk.stopped(), Some(WalkStop::Deadline));
}

#[test]
fn write_atomic_ersetzt_symlink_auf_fremde_datei() {
    let fx = fixture();
    let target = fx.root.join("auth.toml");
    symlink(fx.outside.join("victim.txt"), &target).expect("symlink");

    write_atomic(&target, b"token = 1\n", AtomicWriteOptions::private()).expect("write");

    assert_eq!(fs::read(fx.outside.join("victim.txt")).expect("victim"), b"fremd");
    let meta = fs::symlink_metadata(&target).expect("meta");
    assert!(meta.file_type().is_file());
    assert_eq!(meta.permissions().mode() & 0o7777, 0o600);

    let file = open_nofollow(&target, OpenMode::read_only()).expect("open");
    ensure_private_regular(&file).expect("privat");
    assert_eq!(read_string(file), "token = 1\n");
}

#[test]
fn ensure_private_regular_lehnt_weltlesbare_datei_ab() {
    let fx = fixture();
    let path = fx.root.join("data/secret.txt");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
    let file = open_nofollow(&path, OpenMode::read_only()).expect("open");
    let err = ensure_private_regular(&file).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn open_beneath_legt_nicht_hinter_dangling_symlink_an() {
    let fx = fixture();
    let root = open_dir_nofollow(&fx.root).expect("root");
    let create = OpenMode::write_create_new(0o600);
    let err = open_beneath(root.as_fd(), Path::new("dangling"), create).unwrap_err();
    assert!(errno_in(&err, &[Errno::LOOP, Errno::EXIST]), "{err:?}");
    assert!(!fx.root.join("nirgends").exists());
}

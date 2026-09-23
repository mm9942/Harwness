//! Integrationstests der öffentlichen `harw-fsutil`-API gegen echte
//! Symlink-Fixtures in einem Tempdir.
//!
//! Die Fixtures bilden die Angriffsmuster nach, gegen die das Crate schützen
//! soll: Symlink als letztes Glied, Symlink als Zwischenglied, Ausbruch per
//! `..`/absolutem Pfad, Symlink-Schleifen und Symlinks auf fremde Dateien am
//! Ziel eines atomaren Schreibvorgangs.

mod common;

use std::fs;
use std::io::{self, Read};
use std::os::fd::AsFd;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use common::{TestError, TestResult, ctx};
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

fn fixture() -> TestResult<Fixture> {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("root");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(root.join("data")).map_err(ctx("mkdir data"))?;
    fs::create_dir_all(&outside).map_err(ctx("mkdir outside"))?;
    fs::write(root.join("data/secret.txt"), b"geheim").map_err(ctx("write secret"))?;
    fs::write(outside.join("victim.txt"), b"fremd").map_err(ctx("write victim"))?;
    symlink(".", root.join("data/loop")).map_err(ctx("symlink loop"))?;
    symlink("../outside", root.join("escape")).map_err(ctx("symlink escape"))?;
    symlink("data", root.join("hop")).map_err(ctx("symlink hop"))?;
    symlink("data/secret.txt", root.join("final")).map_err(ctx("symlink final"))?;
    symlink("nirgends", root.join("dangling")).map_err(ctx("symlink dangling"))?;
    Ok(Fixture {
        _tmp: tmp,
        root,
        outside,
    })
}

fn errno_in(err: &io::Error, candidates: &[Errno]) -> bool {
    candidates
        .iter()
        .any(|errno| err.raw_os_error() == Some(errno.raw_os_error()))
}

/// Wie [`errno_in`], aber liefert einen [`TestError`] statt zu assert!en.
fn expect_errno(err: &io::Error, candidates: &[Errno]) -> TestResult {
    if errno_in(err, candidates) {
        Ok(())
    } else {
        Err(TestError::Unexpected(format!("{err:?}")))
    }
}

fn read_string(mut file: fs::File) -> TestResult<String> {
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

#[test]
fn letztes_glied_symlink_wird_nie_gefolgt() -> TestResult {
    let fx = fixture()?;
    let Err(err) = open_nofollow(&fx.root.join("final"), OpenMode::read_only()) else {
        return Err(TestError::Unexpected("Err erwartet".into()));
    };
    expect_errno(&err, &[Errno::LOOP])?;

    let root = open_dir_nofollow(&fx.root).map_err(ctx("root"))?;
    let Err(err) = open_beneath(root.as_fd(), Path::new("final"), OpenMode::read_only()) else {
        return Err(TestError::Unexpected("Err erwartet".into()));
    };
    expect_errno(&err, &[Errno::LOOP])
}

#[test]
fn zwischenglied_symlink_wird_bei_open_beneath_abgelehnt() -> TestResult {
    let fx = fixture()?;
    let root = open_dir_nofollow(&fx.root).map_err(ctx("root"))?;

    let secret = Path::new("data/secret.txt");
    let direct =
        open_beneath(root.as_fd(), secret, OpenMode::read_only()).map_err(ctx("direkt"))?;
    assert_eq!(read_string(direct)?, "geheim");

    for rel in [
        "hop/secret.txt",
        "escape/victim.txt",
        "data/loop/secret.txt",
    ] {
        let Err(err) = open_beneath(root.as_fd(), Path::new(rel), OpenMode::read_only()) else {
            return Err(TestError::Unexpected(format!("Err erwartet für {rel}")));
        };
        if !errno_in(&err, &[Errno::LOOP, Errno::NOTDIR]) {
            return Err(TestError::Unexpected(format!("{rel}: {err:?}")));
        }
    }
    Ok(())
}

#[test]
fn ausbruch_per_punktpunkt_oder_absolut_wird_abgelehnt() -> TestResult {
    let fx = fixture()?;
    let root = open_dir_nofollow(&fx.root).map_err(ctx("root"))?;
    let absolute = fx.outside.join("victim.txt");
    let candidates = [
        Path::new("../outside/victim.txt"),
        Path::new("data/../../outside/victim.txt"),
        absolute.as_path(),
    ];
    for rel in candidates {
        let Err(err) = open_beneath(root.as_fd(), rel, OpenMode::read_only()) else {
            return Err(TestError::Unexpected(format!("Err erwartet für {rel:?}")));
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{rel:?}");
    }
    Ok(())
}

#[test]
fn wurzel_als_symlink_wird_abgelehnt() -> TestResult {
    let fx = fixture()?;
    let Err(err) = open_dir_nofollow(&fx.root.join("hop")) else {
        return Err(TestError::Unexpected("Err erwartet".into()));
    };
    expect_errno(&err, &[Errno::LOOP, Errno::NOTDIR])?;
    let Err(_) = walk_beneath(
        &fx.root.join("hop"),
        WalkLimits {
            max_depth: 4,
            max_entries: 100,
            deadline: None,
        },
    ) else {
        return Err(TestError::Unexpected("Err erwartet".into()));
    };
    Ok(())
}

#[test]
fn walk_folgt_keinen_symlinks_und_terminiert() -> TestResult {
    let fx = fixture()?;
    let limits = WalkLimits {
        max_depth: usize::MAX,
        max_entries: 1_000,
        deadline: None,
    };
    let mut walk = walk_beneath(&fx.root, limits).map_err(ctx("walk"))?;
    let entries: Vec<WalkEntry> = walk.by_ref().collect::<io::Result<Vec<_>>>()?;
    let summary: Vec<(&str, EntryType)> = entries
        .iter()
        .map(|entry| {
            Ok::<_, TestError>((
                entry.rel_path.to_str().ok_or(TestError::Missing("utf8"))?,
                entry.entry_type,
            ))
        })
        .collect::<TestResult<Vec<_>>>()?;
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
    Ok(())
}

#[test]
fn walk_grenzen_melden_stopped() -> TestResult {
    let fx = fixture()?;
    let depth = WalkLimits {
        max_depth: 1,
        max_entries: 1_000,
        deadline: None,
    };
    let mut walk = walk_beneath(&fx.root, depth).map_err(ctx("walk"))?;
    assert_eq!(walk.by_ref().count(), 5);
    assert_eq!(walk.stopped(), Some(WalkStop::DepthLimit));

    let entries = WalkLimits {
        max_depth: usize::MAX,
        max_entries: 2,
        deadline: None,
    };
    let mut walk = walk_beneath(&fx.root, entries).map_err(ctx("walk"))?;
    assert_eq!(walk.by_ref().count(), 2);
    assert_eq!(walk.stopped(), Some(WalkStop::EntryLimit));

    let deadline = WalkLimits {
        max_depth: usize::MAX,
        max_entries: 1_000,
        deadline: Some(std::time::Instant::now()),
    };
    let mut walk = walk_beneath(&fx.root, deadline).map_err(ctx("walk"))?;
    assert_eq!(walk.by_ref().count(), 0);
    assert_eq!(walk.stopped(), Some(WalkStop::Deadline));
    Ok(())
}

#[test]
fn write_atomic_ersetzt_symlink_auf_fremde_datei() -> TestResult {
    let fx = fixture()?;
    let target = fx.root.join("auth.toml");
    symlink(fx.outside.join("victim.txt"), &target)?;

    write_atomic(&target, b"token = 1\n", AtomicWriteOptions::private()).map_err(ctx("write"))?;

    assert_eq!(
        fs::read(fx.outside.join("victim.txt")).map_err(ctx("victim"))?,
        b"fremd"
    );
    let meta = fs::symlink_metadata(&target)?;
    assert!(meta.file_type().is_file());
    assert_eq!(meta.permissions().mode() & 0o7777, 0o600);

    let file = open_nofollow(&target, OpenMode::read_only()).map_err(ctx("open"))?;
    ensure_private_regular(&file).map_err(ctx("privat"))?;
    assert_eq!(read_string(file)?, "token = 1\n");
    Ok(())
}

#[test]
fn ensure_private_regular_lehnt_weltlesbare_datei_ab() -> TestResult {
    let fx = fixture()?;
    let path = fx.root.join("data/secret.txt");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    let file = open_nofollow(&path, OpenMode::read_only()).map_err(ctx("open"))?;
    let Err(err) = ensure_private_regular(&file) else {
        return Err(TestError::Unexpected("Err erwartet".into()));
    };
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    Ok(())
}

#[test]
fn open_beneath_legt_nicht_hinter_dangling_symlink_an() -> TestResult {
    let fx = fixture()?;
    let root = open_dir_nofollow(&fx.root).map_err(ctx("root"))?;
    let create = OpenMode::write_create_new(0o600);
    let Err(err) = open_beneath(root.as_fd(), Path::new("dangling"), create) else {
        return Err(TestError::Unexpected("Err erwartet".into()));
    };
    expect_errno(&err, &[Errno::LOOP, Errno::EXIST])?;
    assert!(!fx.root.join("nirgends").exists());
    Ok(())
}

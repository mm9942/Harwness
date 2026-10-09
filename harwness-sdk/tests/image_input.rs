//! Bilder über die öffentliche Fläche: Offline-Echo, kein Netz.
//!
//! Der Echo-Provider sieht keine Bilder; geprüft wird hier, was die SDK davor
//! leistet: Prüfen, Säubern, Ablegen unter dem Home, Grenzen.

use std::path::{Path, PathBuf};

use harwness_sdk::prelude::*;
use harwness_sdk::{MAX_IMAGES_PER_MESSAGE, TurnStatus};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const TAIL: &[u8] = b"APPENDED-SECRET-TAIL";

struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

fn fixture() -> TestResult<Fixture> {
    let dir = tempfile::tempdir()?;
    let home = dir.path().join("home");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&project)?;
    std::fs::write(project.join("Cargo.toml"), "[workspace]\n")?;
    let agent_dir = home.join("profiles/default/agents/fixture-uia");
    std::fs::create_dir_all(&agent_dir)?;
    std::fs::write(
        agent_dir.join("definition.toml"),
        "schema = \"harwness.agent/v1\"\nid = \"harwness.agent.fixture-uia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\n",
    )?;
    std::fs::write(
        home.join("profiles/default/config.toml"),
        "active_uia_definition = \"harwness.agent.fixture-uia@1\"\n",
    )?;
    Ok(Fixture {
        _dir: dir,
        home,
        project,
    })
}

fn builder(fixture: &Fixture) -> HarwnessBuilder {
    Harwness::builder()
        .home(&fixture.home)
        .cwd(&fixture.project)
        .scaffold_home(false)
        .ephemeral(true)
        .offline_echo("seen")
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(data.len())
        .unwrap_or(0)
        .to_be_bytes()
        .to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&[0; 4]);
    out
}

/// Ein gültiger PNG-Container um Platzhalter-Pixel, mit Anhang hinter IEND.
fn png_with_tail() -> Vec<u8> {
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = 4u32.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&4u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    out.extend_from_slice(&chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&chunk(b"IDAT", b"pixel-data"));
    out.extend_from_slice(&chunk(b"IEND", b""));
    out.extend_from_slice(TAIL);
    out
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            files_under(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test]
async fn an_image_is_stored_without_its_appended_data_and_the_turn_completes() -> TestResult {
    let fixture = fixture()?;
    let harwness = builder(&fixture).build()?;
    let mut session = harwness.session()?;

    let report = session
        .send_with_images("what is this?", vec![Image::from_bytes(png_with_tail())])
        .await?;
    assert_eq!(report.status, TurnStatus::Completed);

    let mut files = Vec::new();
    files_under(&fixture.home.join("media"), &mut files)?;
    let stored: Vec<Vec<u8>> = files.iter().map(std::fs::read).collect::<Result<_, _>>()?;
    assert!(
        stored.iter().any(|bytes| bytes.starts_with(b"\x89PNG")),
        "the image is in the store"
    );
    assert!(
        stored.iter().all(|bytes| !contains(bytes, TAIL)),
        "appended data never reaches the store"
    );
    Ok(())
}

#[tokio::test]
async fn an_image_alone_is_a_message_but_nothing_at_all_is_not() -> TestResult {
    let fixture = fixture()?;
    let harwness = builder(&fixture).build()?;
    let mut session = harwness.session()?;

    let report = session
        .send_with_images("", vec![Image::from_bytes(png_with_tail())])
        .await?;
    assert_eq!(report.status, TurnStatus::Completed);

    let empty = session.send_with_images("  ", Vec::new()).await;
    assert!(matches!(empty, Err(SdkError::InvalidInput { .. })));
    Ok(())
}

#[tokio::test]
async fn bad_images_and_too_many_images_are_refused_before_any_turn() -> TestResult {
    let fixture = fixture()?;
    let harwness = builder(&fixture).build()?;
    let mut session = harwness.session()?;

    let not_an_image = session
        .send_with_images("look", vec![Image::from_bytes(b"just text".to_vec())])
        .await;
    assert!(matches!(not_an_image, Err(SdkError::InvalidInput { .. })));

    let many = (0..=MAX_IMAGES_PER_MESSAGE)
        .map(|_| Image::from_bytes(png_with_tail()))
        .collect();
    let too_many = session.send_with_images("look", many).await;
    assert!(matches!(too_many, Err(SdkError::InvalidInput { .. })));

    // Nichts davon hat das Home mit einem Medienspeicher belegt.
    let mut files = Vec::new();
    files_under(&fixture.home.join("media"), &mut files)?;
    assert!(files.is_empty());
    Ok(())
}

#[test]
fn a_file_is_read_with_a_limit_and_debug_never_shows_bytes() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("a.png");
    std::fs::write(&path, png_with_tail())?;
    let image = Image::from_file(&path)?;
    let shown = format!("{image:?}");
    assert!(shown.contains("bytes"));
    assert!(!shown.contains("PNG"));

    assert!(
        Image::from_file(dir.path()).is_err(),
        "a directory is no image"
    );
    assert!(Image::from_file(dir.path().join("missing.png")).is_err());
    Ok(())
}

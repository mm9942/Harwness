//! Live Tor-network integration test for the onion connector.
//!
//! It launches a real onion service, dials it from a second, independent Tor
//! client and checks an echo round trip. Afterwards it relaunches the service
//! on the same state directory and asserts that the `.onion` address is stable.
//!
//! Manual run (needs internet, takes several minutes, uses temp dirs only):
//!
//! ```text
//! CARGO_TARGET_DIR=/root/harw-target cargo test -p harw-onion --features service \
//!     --test live_onion -- --ignored --nocapture
//! ```

use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use harw_onion::{OnionClient, OnionClientConfig, OnionServer, OnionServerConfig, OnionStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const PING: &[u8] = b"harw-onion-ping";

/// Creates a private (0700) directory tree and returns the leaf path.
fn private_dir(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    Ok(path.to_path_buf())
}

/// Echoes up to 64 bytes back to the peer.
async fn echo(mut stream: OnionStream) {
    let mut buf = [0u8; 64];
    if let Ok(n) = stream.read(&mut buf).await {
        if stream.write_all(&buf[..n]).await.is_ok() {
            let _ = stream.flush().await;
        }
    }
}

async fn launch_server(state: &Path, cache: &Path) -> Result<OnionServer, harw_onion::OnionError> {
    let config = OnionServerConfig::new(state, cache);
    OnionServer::launch(
        config,
        |p| eprintln!("server tor {}%", p.percent),
        echo,
    )
    .await
}

#[ignore = "needs the live Tor network"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn launches_an_onion_service_and_dials_it() -> TestResult {
    let dir = tempfile::tempdir()?;
    let server_state = private_dir(&dir.path().join("server/state"))?;
    let server_cache = private_dir(&dir.path().join("server/cache"))?;
    let client_state = private_dir(&dir.path().join("client/state"))?;
    let client_cache = private_dir(&dir.path().join("client/cache"))?;

    let server = launch_server(&server_state, &server_cache).await?;
    let locator = server.locator().clone();
    eprintln!("onion service {}", locator.redacted());
    assert!(
        server.wait_reachable(Duration::from_secs(300)).await,
        "onion service did not become reachable"
    );

    let client = OnionClient::launch(&OnionClientConfig::new(&client_state, &client_cache))?;
    let mut stream = client.dial(&locator, Duration::from_secs(180)).await?;
    stream.write_all(PING).await?;
    stream.flush().await?;
    let mut echoed = vec![0u8; PING.len()];
    stream.read_exact(&mut echoed).await?;
    assert_eq!(echoed, PING);
    drop(stream);
    drop(client);

    server.shutdown();
    let relaunched = launch_server(&server_state, &server_cache).await?;
    assert_eq!(
        relaunched.locator(),
        &locator,
        "the onion address must persist with the state directory"
    );
    eprintln!("relaunched onion service {}", relaunched.locator().redacted());
    relaunched.shutdown();
    Ok(())
}

//! `harw-mobile`: attach to a hosted session from a phone terminal.
//!
//! ```text
//! harw-mobile [--socket PATH] [--session ID | --new [--title TEXT]]
//! ```
//!
//! The default socket is the one `harw gateway --session-socket` and
//! `harw attach` use: `$XDG_RUNTIME_DIR/harw/session.sock`. Without
//! `--session` the first hosted session is used; `--new` creates one.

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use harw_mobile_core::Controller;
use harw_mobile_term::args::{Args, parse_args};
use harw_mobile_term::{RunEnd, run};
use harw_protocol::SessionPort;
use harw_protocol::session_wire::CreateParams;
use harw_session_remote::{ConnectOptions, RemotePort, connect_unix};
use harw_types::SessionId;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

async fn session_id(port: &Arc<dyn SessionPort>, args: &Args) -> Result<SessionId, String> {
    if let Some(id) = &args.session {
        return Ok(SessionId::from_str(id.clone()));
    }
    if args.new {
        let created = port
            .create(CreateParams {
                workspace: None,
                title: args.title.clone(),
            })
            .await
            .map_err(|e| format!("create: {e}"))?;
        return Ok(created.session_id);
    }
    let sessions = port.list().await.map_err(|e| format!("list: {e}"))?;
    sessions
        .into_iter()
        .next()
        .map(|s| s.session_id)
        .ok_or_else(|| "no hosted session; start one with --new".to_owned())
}

async fn real_main() -> Result<(), String> {
    let args = parse_args(
        std::env::args().skip(1),
        std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
    )?;
    let connection = connect_unix(args.socket.clone(), ConnectOptions::new("harw-mobile"))
        .await
        .map_err(|e| format!("{}: {e}", args.socket.display()))?;
    let port: Arc<dyn SessionPort> = Arc::new(RemotePort::new(connection));
    let session = session_id(&port, &args).await?;
    let mut controller = Controller::new(Arc::clone(&port), "harw-mobile");
    controller
        .attach(session, None)
        .await
        .map_err(|e| format!("attach: {e}"))?;

    let (tx, rx) = mpsc::channel::<String>(16);
    tokio::spawn(async move {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if tx.send(line).await.is_err() {
                break;
            }
        }
    });

    let mut out = std::io::stdout();
    let _ = writeln!(out, "attached; /help lists the commands");
    match run(&mut controller, rx, &mut out).await {
        Ok(RunEnd::Quit | RunEnd::InputClosed) => Ok(()),
        Ok(RunEnd::Lost(why)) => Err(format!("connection lost: {why}")),
        Err(error) => Err(format!("output: {error}")),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match real_main().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("harw-mobile: {message}");
            ExitCode::FAILURE
        }
    }
}

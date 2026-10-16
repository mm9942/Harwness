//! Grammatik von `harw attach` (W00 W04/W06, Scope S09).
//!
//! Skelett: nur die Argumentform ist eingefroren; `attach_cmd::run` meldet
//! noch „nicht implementiert“.

use std::path::PathBuf;

use clap::{Args, ValueHint};

/// Argumente von `harw attach`: an eine laufende Host-Sitzung anhängen.
#[derive(Debug, Args)]
pub struct AttachArgs {
    /// Sitzungs-ID oder Präfix; ohne Angabe wird die Sitzungsliste des Hosts
    /// angeboten.
    #[arg(value_name = "SESSION", value_hint = ValueHint::Other)]
    pub session: Option<String>,
    /// Lokalen Host-Socket (AF_UNIX) statt des Standard-Sockets verwenden.
    #[arg(long, value_name = "PATH", value_hint = ValueHint::FilePath, conflicts_with = "host")]
    pub socket: Option<PathBuf>,
    /// Alias eines Own-Cloud-Hosts aus dem lokalen Host-Alias-Speicher.
    #[arg(long, value_name = "ALIAS", value_hint = ValueHint::Other)]
    pub host: Option<String>,
}

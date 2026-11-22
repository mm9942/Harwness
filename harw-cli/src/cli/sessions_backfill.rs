//! Grammatik von `harw session backfill-projects`: ordnet alten Sitzungen
//! einmalig ein Projekt zu.

use clap::Args;

/// `harw session backfill-projects [--dry-run] [--since <DATUM>]`.
#[derive(Debug, Clone, Args)]
pub struct BackfillProjectsArgs {
    /// Nur zählen, nichts schreiben.
    #[arg(long)]
    pub dry_run: bool,
    /// Nur Sessions ab diesem Datum (YYYY-MM-DD).
    #[arg(long, value_name = "DATUM")]
    pub since: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::BackfillProjectsArgs;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct Wrapper {
        #[command(flatten)]
        args: BackfillProjectsArgs,
    }

    #[test]
    fn parses_defaults() {
        let w = Wrapper::try_parse_from(["t"]).expect("parse");
        assert!(!w.args.dry_run);
        assert!(w.args.since.is_none());
    }

    #[test]
    fn parses_dry_run_and_since() {
        let w =
            Wrapper::try_parse_from(["t", "--dry-run", "--since", "2026-01-31"]).expect("parse");
        assert!(w.args.dry_run);
        assert_eq!(w.args.since.as_deref(), Some("2026-01-31"));
    }

    #[test]
    fn rejects_unknown_flag() {
        assert!(Wrapper::try_parse_from(["t", "--write"]).is_err());
    }
}

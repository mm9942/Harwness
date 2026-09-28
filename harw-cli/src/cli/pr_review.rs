//! clap-Grammatik von `harw pr-review` (R3, Plan harw-github-pr-reviewer).
//!
//! Alle PR-/Repo-Werte bleiben argv-Werte (`Command::new("gh")` mit
//! `.arg()`); es gibt keinen Shell-String, keinen Interpolationspfad.
//! `--post` verlangt zusätzlich die interaktive Bestätigung im Runner.

use std::path::PathBuf;

use clap::{Parser, ValueHint};

use crate::pr_review::DEFAULT_MAX_DIFF_BYTES;

/// Argumente von `harw pr-review` (sicherer lokaler PR-Review-Runner).
#[derive(Debug, Clone, Parser)]
pub struct PrReviewArgs {
    /// GitHub-PR-Nummer (read-only gelesen; argv-Wert, kein Shell-Text).
    #[arg(value_name = "PR", value_parser = clap::value_parser!(u64))]
    pub pr: u64,

    /// Ziel-Repository als `owner/name`; ohne Angabe wählt `gh` das Current.
    #[arg(long, value_name = "OWNER/REPO", value_hint = ValueHint::Other)]
    pub repo: Option<String>,

    /// Maximalgröße des Diffs in KiB (Vorgabe 512); übergroß = Abbruch.
    #[arg(long = "max-diff-kib", default_value_t = DEFAULT_MAX_DIFF_BYTES / 1024, value_name = "KIB")]
    pub max_diff_kib: usize,

    /// Ablagepfad des Diffs (Vorgabe: scratch/pr-reviewer/fixture.diff).
    #[arg(long, value_name = "PFAD", value_hint = ValueHint::AnyPath, default_value = "scratch/pr-reviewer/fixture.diff")]
    pub fixture: PathBuf,

    /// Nur den Diff holen und als Fixture ablegen (Diagnose, kein Agenten-Lauf).
    #[arg(long)]
    pub fixture_only: bool,

    /// Findings-Bericht in diese Datei schreiben; ohne Angabe nach stdout.
    #[arg(short = 'o', long = "output", value_name = "PFAD", value_hint = ValueHint::AnyPath)]
    pub output: Option<PathBuf>,

    /// Veröffentliche Findings als GitHub-Kommentar — NUR nach separater,
    /// interaktiver Bestätigung im Runner; bis R5 bewusst nicht implementiert.
    #[arg(long)]
    pub post: bool,
}

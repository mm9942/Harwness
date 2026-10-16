//! Validierte Auswahl-Optionen und deren Übersetzung in `argv`.
//!
//! [`Selection`] fasst die Optionen zusammen, die mehrere Werkzeuge teilen
//! (`-p`, `--workspace`, `--exclude`, `--features`, `--release`, `--locked`,
//! `--offline`, `-j`, …). Jede Methode `*_args` hängt nur Argumente an, die das
//! jeweilige Cargo-Unterkommando kennt; die Werkzeug-Argumentstrukturen
//! bieten nur die passenden Felder an, sodass eine unpassende Option gar nicht
//! erst als Feld existiert (und wegen `deny_unknown_fields` abgelehnt wird).

use crate::command;

/// Standard-Zeitlimit in Sekunden.
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Größtes Zeitlimit in Sekunden.
pub const MAX_TIMEOUT_SECS: u64 = 3_600;

/// Standard-Zahl der gemeldeten Meldungen.
pub const DEFAULT_DIAGNOSTICS: usize = 50;

/// Größte Zahl gemeldeter Meldungen.
pub const MAX_DIAGNOSTICS: usize = 500;

/// Rohe (noch ungeprüfte) Auswahl, wie sie aus den Argumenten kommt.
#[derive(Debug, Clone, Default)]
pub struct RawSelection {
    /// `-p`/`--package`.
    pub package: Option<Vec<String>>,
    /// `--workspace`.
    pub workspace: Option<bool>,
    /// `--exclude`.
    pub exclude: Option<Vec<String>>,
    /// `--features`.
    pub features: Option<Vec<String>>,
    /// `--all-features`.
    pub all_features: Option<bool>,
    /// `--no-default-features`.
    pub no_default_features: Option<bool>,
    /// `--release`.
    pub release: Option<bool>,
    /// `--profile`.
    pub profile: Option<String>,
    /// `--locked`.
    pub locked: Option<bool>,
    /// `--offline`.
    pub offline: Option<bool>,
    /// `-j`.
    pub jobs: Option<u64>,
    /// Zeitlimit.
    pub timeout_secs: Option<u64>,
    /// Zahl der gemeldeten Meldungen.
    pub max_diagnostics: Option<usize>,
}

/// Rohe Zielauswahl.
#[derive(Debug, Clone, Default)]
pub struct RawTargets {
    /// `--all-targets`.
    pub all_targets: Option<bool>,
    /// `--lib`.
    pub lib: Option<bool>,
    /// `--bins`.
    pub bins: Option<bool>,
    /// `--tests`.
    pub tests: Option<bool>,
    /// `--examples`.
    pub examples: Option<bool>,
    /// `--benches`.
    pub benches: Option<bool>,
    /// `--bin`.
    pub bin: Option<Vec<String>>,
}

/// Geprüfte Auswahl.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    packages: Vec<String>,
    workspace: bool,
    exclude: Vec<String>,
    features: Vec<String>,
    all_features: bool,
    no_default_features: bool,
    release: bool,
    profile: Option<String>,
    locked: bool,
    offline: bool,
    jobs: Option<u32>,
    /// Zeitlimit in Sekunden.
    pub timeout_secs: u64,
    /// Zahl der gemeldeten Meldungen.
    pub max_diagnostics: usize,
}

fn flag(value: Option<bool>) -> bool {
    value.unwrap_or(false)
}

fn list<F>(kind: &str, values: Option<&Vec<String>>, check: F) -> Result<Vec<String>, String>
where
    F: Fn(&str) -> Result<(), String>,
{
    let Some(values) = values else {
        return Ok(Vec::new());
    };
    if values.len() > command::MAX_LIST {
        return Err(format!(
            "too many {kind} entries (max {})",
            command::MAX_LIST
        ));
    }
    let mut out: Vec<String> = Vec::new();
    for value in values {
        check(value)?;
        if !out.contains(value) {
            out.push(value.clone());
        }
    }
    Ok(out)
}

impl RawSelection {
    /// Prüft die Auswahl.
    ///
    /// # Errors
    /// Meldung bei ungültigen Werten oder widersprüchlichen Optionen.
    pub fn validate(&self) -> Result<Selection, String> {
        let packages = list("package", self.package.as_ref(), command::package)?;
        let exclude = list("exclude", self.exclude.as_ref(), command::package)?;
        let features = list("features", self.features.as_ref(), command::feature)?;
        let workspace = flag(self.workspace);
        if !exclude.is_empty() && !workspace {
            return Err("exclude can only be used together with workspace=true".to_owned());
        }
        let release = flag(self.release);
        if let Some(profile) = &self.profile {
            command::profile(profile)?;
            if release {
                return Err("release and profile are mutually exclusive".to_owned());
            }
        }
        let jobs = match self.jobs {
            None => None,
            Some(n) if (1..=64).contains(&n) => u32::try_from(n).ok(),
            Some(n) => return Err(format!("jobs must be between 1 and 64, got {n}")),
        };
        let timeout_secs = match self.timeout_secs {
            None => DEFAULT_TIMEOUT_SECS,
            Some(n) if (1..=MAX_TIMEOUT_SECS).contains(&n) => n,
            Some(n) => {
                return Err(format!(
                    "timeout_secs must be between 1 and {MAX_TIMEOUT_SECS}, got {n}"
                ));
            }
        };
        let max_diagnostics = match self.max_diagnostics {
            None => DEFAULT_DIAGNOSTICS,
            Some(0) => return Err("max_diagnostics must be at least 1".to_owned()),
            Some(n) => n.min(MAX_DIAGNOSTICS),
        };
        Ok(Selection {
            packages,
            workspace,
            exclude,
            features,
            all_features: flag(self.all_features),
            no_default_features: flag(self.no_default_features),
            release,
            profile: self.profile.clone(),
            locked: flag(self.locked),
            offline: flag(self.offline),
            jobs,
            timeout_secs,
            max_diagnostics,
        })
    }
}

impl Selection {
    /// Gibt es eine ausdrückliche Paketauswahl?
    #[must_use]
    pub fn has_packages(&self) -> bool {
        !self.packages.is_empty() || self.workspace
    }

    /// `-p`, `--workspace`, `--exclude`.
    pub fn package_args(&self, argv: &mut Vec<String>) {
        for package in &self.packages {
            argv.push("-p".to_owned());
            argv.push(package.clone());
        }
        if self.workspace {
            argv.push("--workspace".to_owned());
        }
        for package in &self.exclude {
            argv.push("--exclude".to_owned());
            argv.push(package.clone());
        }
    }

    /// `--features`, `--all-features`, `--no-default-features`.
    pub fn feature_args(&self, argv: &mut Vec<String>) {
        if !self.features.is_empty() {
            argv.push("--features".to_owned());
            argv.push(self.features.join(","));
        }
        if self.all_features {
            argv.push("--all-features".to_owned());
        }
        if self.no_default_features {
            argv.push("--no-default-features".to_owned());
        }
    }

    /// `--release` oder `--profile`.
    pub fn profile_args(&self, argv: &mut Vec<String>) {
        if self.release {
            argv.push("--release".to_owned());
        }
        if let Some(profile) = &self.profile {
            argv.push("--profile".to_owned());
            argv.push(profile.clone());
        }
    }

    /// `--locked`, `--offline`.
    pub fn net_args(&self, argv: &mut Vec<String>) {
        if self.locked {
            argv.push("--locked".to_owned());
        }
        if self.offline {
            argv.push("--offline".to_owned());
        }
    }

    /// `-j N`.
    pub fn jobs_args(&self, argv: &mut Vec<String>) {
        if let Some(jobs) = self.jobs {
            argv.push("-j".to_owned());
            argv.push(jobs.to_string());
        }
    }
}

/// Geprüfte Zielauswahl.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Targets {
    all_targets: bool,
    lib: bool,
    bins: bool,
    tests: bool,
    examples: bool,
    benches: bool,
    bin: Vec<String>,
}

impl RawTargets {
    /// Prüft die Zielauswahl.
    ///
    /// # Errors
    /// Meldung bei ungültigen Namen.
    pub fn validate(&self) -> Result<Targets, String> {
        Ok(Targets {
            all_targets: flag(self.all_targets),
            lib: flag(self.lib),
            bins: flag(self.bins),
            tests: flag(self.tests),
            examples: flag(self.examples),
            benches: flag(self.benches),
            bin: list("bin", self.bin.as_ref(), command::target_name)?,
        })
    }
}

impl Targets {
    /// `--all-targets`, `--lib`, `--bins`, `--tests`, `--examples`, `--benches`, `--bin`.
    pub fn args(&self, argv: &mut Vec<String>) {
        for (set, name) in [
            (self.all_targets, "--all-targets"),
            (self.lib, "--lib"),
            (self.bins, "--bins"),
            (self.tests, "--tests"),
            (self.examples, "--examples"),
            (self.benches, "--benches"),
        ] {
            if set {
                argv.push(name.to_owned());
            }
        }
        for bin in &self.bin {
            argv.push("--bin".to_owned());
            argv.push(bin.clone());
        }
    }
}

/// Ein fertiger Plan: Umgebungs-Präfix und `argv`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// `NAME=wert`-Präfixe (feste Werte, nie aus Argumenten).
    pub env: Vec<(&'static str, &'static str)>,
    /// Das Programm und seine Argumente.
    pub argv: Vec<String>,
}

impl Plan {
    /// Neuer Plan für `cargo <sub>`.
    #[must_use]
    pub fn cargo(sub: &str) -> Self {
        Self {
            env: Vec::new(),
            argv: vec!["cargo".to_owned(), sub.to_owned()],
        }
    }

    /// Hängt Argumente an.
    pub fn extend<I: IntoIterator<Item = S>, S: Into<String>>(&mut self, items: I) {
        self.argv.extend(items.into_iter().map(Into::into));
    }

    /// Der Kommando-String für `shell.exec`.
    #[must_use]
    pub fn command(&self) -> String {
        let mut parts: Vec<String> = self.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
        parts.push(command::join(&self.argv));
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn names(items: &[&str]) -> Option<Vec<String>> {
        Some(items.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn builds_arguments_in_a_stable_order() -> TestResult {
        let raw = RawSelection {
            package: names(&["harw-core", "harw-tools", "harw-core"]),
            workspace: Some(true),
            exclude: names(&["harw-web"]),
            features: names(&["a", "b/c"]),
            all_features: Some(true),
            no_default_features: Some(true),
            release: Some(true),
            locked: Some(true),
            offline: Some(true),
            jobs: Some(4),
            ..RawSelection::default()
        };
        let selection = raw.validate().map_err(TestError::Unexpected)?;
        let mut argv = Vec::new();
        selection.package_args(&mut argv);
        selection.feature_args(&mut argv);
        selection.profile_args(&mut argv);
        selection.net_args(&mut argv);
        selection.jobs_args(&mut argv);
        assert_eq!(
            argv.join(" "),
            "-p harw-core -p harw-tools --workspace --exclude harw-web --features a,b/c --all-features --no-default-features --release --locked --offline -j 4"
        );
        assert_eq!(selection.timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert!(selection.has_packages());
        Ok(())
    }

    #[test]
    fn targets_args() -> TestResult {
        let raw = RawTargets {
            all_targets: Some(true),
            lib: Some(true),
            bins: Some(true),
            tests: Some(true),
            examples: Some(true),
            benches: Some(true),
            bin: names(&["app"]),
        };
        let targets = raw.validate().map_err(TestError::Unexpected)?;
        let mut argv = Vec::new();
        targets.args(&mut argv);
        assert_eq!(
            argv.join(" "),
            "--all-targets --lib --bins --tests --examples --benches --bin app"
        );
        assert!(
            RawTargets {
                bin: names(&["a b"]),
                ..RawTargets::default()
            }
            .validate()
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn conflicts_and_bounds_are_rejected() -> TestResult {
        let bad = [
            RawSelection {
                release: Some(true),
                profile: Some("dev".into()),
                ..RawSelection::default()
            },
            RawSelection {
                exclude: names(&["x"]),
                ..RawSelection::default()
            },
            RawSelection {
                jobs: Some(0),
                ..RawSelection::default()
            },
            RawSelection {
                jobs: Some(65),
                ..RawSelection::default()
            },
            RawSelection {
                jobs: Some(u64::MAX),
                ..RawSelection::default()
            },
            RawSelection {
                timeout_secs: Some(0),
                ..RawSelection::default()
            },
            RawSelection {
                timeout_secs: Some(MAX_TIMEOUT_SECS + 1),
                ..RawSelection::default()
            },
            RawSelection {
                max_diagnostics: Some(0),
                ..RawSelection::default()
            },
            RawSelection {
                package: names(&["a;b"]),
                ..RawSelection::default()
            },
            RawSelection {
                package: names(&["--config=x"]),
                ..RawSelection::default()
            },
            RawSelection {
                features: names(&["a b"]),
                ..RawSelection::default()
            },
            RawSelection {
                profile: Some("a b".into()),
                ..RawSelection::default()
            },
            RawSelection {
                package: Some((0..65).map(|i| format!("p{i}")).collect()),
                ..RawSelection::default()
            },
        ];
        for (index, raw) in bad.iter().enumerate() {
            assert!(raw.validate().is_err(), "case {index} must be rejected");
        }
        let capped = RawSelection {
            max_diagnostics: Some(usize::MAX),
            ..RawSelection::default()
        }
        .validate()
        .map_err(TestError::Unexpected)?;
        assert_eq!(capped.max_diagnostics, MAX_DIAGNOSTICS);
        Ok(())
    }

    #[test]
    fn plan_command_quotes_and_prefixes() -> TestResult {
        let mut plan = Plan::cargo("fmt");
        plan.env.push(("NO_COLOR", "1"));
        plan.extend(["--check", "-p", "a b"]);
        assert_eq!(plan.command(), "NO_COLOR=1 cargo fmt --check -p 'a b'");
        Ok(())
    }
}

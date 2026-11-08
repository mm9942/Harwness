//! Staged-install contract of the Harw-DoD package (PL-100 WP1.8, owner
//! review 4505223360).
//!
//! # Purpose
//! `harw-dod/scripts/install.sh` ships the DoD package from hand-written
//! lists: the packaging manifest `harw-dod/packaging/manifest` (which also
//! drives `install.sh uninstall`), the `dod_units` string and the
//! `for binary in …` / `for object in …` loops of the script. When these
//! lists drift, a unit ships without the binary its `ExecStart=` runs (or a
//! binary without its unit), and that only shows on a real host at
//! `systemctl start`. These tests tie every list to the single source of
//! truth in `harw-install` ([`harw_install::DOD_PACKAGE_BINARIES`],
//! [`harw_install::DOD_PACKAGE_ASSETS`], [`harw_install::UNIT_CLASSES`]) and
//! then run the installer into a `DESTDIR` stage.
//!
//! # What is checked
//! - The manifest `libexec/` entries are [`harw_install::DOD_PACKAGE_BINARIES`]
//!   and its `systemd/`, `sysusers/` and `tmpfiles/` entries are
//!   [`harw_install::DOD_PACKAGE_ASSETS`], each in the same order.
//! - `install.sh`: `dod_units`, both `for binary in` loops and both
//!   `for object in` loops repeat the manifest lists in manifest order, and
//!   no `systemctl` call enables or starts a unit (WP3.4: install activates
//!   no service).
//! - The uninstall path (`install.sh uninstall`) is driven by the manifest,
//!   has a `case` arm for every manifest entry, keeps no list of its own and,
//!   on a host only, stops and disables every shipped unit (one
//!   `systemctl disable --now` per unit of `dod_units`, so the target and the
//!   opt-in probes `harw-probe-fs`/`harw-probe-netlink` included) before the
//!   unit files go away.
//! - Every shipped service's `ExecStart=`, rendered with the installer
//!   defaults, is `@LIBEXECDIR@/<binary>` for a binary the manifest ships
//!   (any other `Exec*=` program in `@LIBEXECDIR@` must be shipped too);
//!   every shipped binary that has a unit under `deploy/systemd/` ships that
//!   unit; every `harw-*` unit a shipped unit names is shipped.
//! - `harw-dod/Makefile` (`build`, the first half of `make install`) builds
//!   every shipped binary.
//! - `harw-probe-netlink` is in all of these lists, but neither
//!   `harw-dod.target` nor any other shipped unit pulls it in, and no DoD
//!   packaging entry point enables it (decision O11).
//! - Staged (Linux only): `install.sh install` with `DESTDIR` installs
//!   exactly the manifest files; every installed unit, sysusers and tmpfiles
//!   snippet equals its embedded copy rendered like
//!   `harw install --print-systemd`, contains no placeholder, and every
//!   libexec program an installed unit runs is an executable file inside the
//!   stage; `install.sh uninstall` then removes every installed file.
//! - Host path of the uninstall (Linux only): `install.sh uninstall` with an
//!   empty `DESTDIR` against a staged install runs `systemctl disable --now`
//!   once per shipped unit, in manifest order and while the unit file still
//!   exists, then `daemon-reload`, and removes every installed file.
//!
//! # Concurrency
//! The static tests only read repository files. The staged tests work in
//! their own temporary directory ([`harw_test_support::unique_tmp_created`]),
//! which the stage's `Drop` removes also when a check fails, and never
//! touch the host: with `DESTDIR` set, `install.sh` skips the root check,
//! `systemd-sysusers`, `systemd-tmpfiles` and every `systemctl` call. The
//! host-path uninstall run sets every directory variable of `install.sh`
//! into the stage and a `PATH` of only a fake `id` (uid 0), a fake
//! `systemctl` (logs its calls) and links to `dirname` and `rm`, so any
//! other program a later host branch would run fails the test instead of
//! running on the build host.
//!
//! # Errors
//! Every failure is a [`TestError`]; checks use `assert!`/`assert_eq!`.
//!
//! # Examples
//! ```ignore
//! // Runs as an integration test of harw-install with the rest of the suite.
//! ```

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use harw_install::deployment::render;
use harw_install::dod_units::{ParsedUnit, parse_unit};
use harw_install::{DOD_PACKAGE_ASSETS, DOD_PACKAGE_BINARIES, RenderPaths, UNIT_CLASSES};

harw_test_support::define_test_error!(pub(crate));

/// The audit netlink probe: shipped with the package, but opt-in (O11).
const NETLINK_PROBE: &str = "harw-probe-netlink";

/// The unit of [`NETLINK_PROBE`].
const NETLINK_UNIT: &str = "harw-probe-netlink.service";

/// The logical prefixes that `install.sh uninstall` maps to a directory.
const MANIFEST_PREFIXES: &[&str] = &[
    "libexec", "bpf", "systemd", "sysusers", "tmpfiles", "config",
];

/// The `[Service]` keys that start a program.
const EXEC_KEYS: &[&str] = &[
    "ExecCondition",
    "ExecStartPre",
    "ExecStart",
    "ExecStartPost",
    "ExecReload",
    "ExecStop",
    "ExecStopPost",
];

/// Directives that name another unit, as `(section, key)`.
const UNIT_REFERENCES: &[(&str, &str)] = &[
    ("Unit", "Wants"),
    ("Unit", "Requires"),
    ("Unit", "Requisite"),
    ("Unit", "BindsTo"),
    ("Unit", "PartOf"),
    ("Unit", "Upholds"),
    ("Service", "Sockets"),
    ("Socket", "Service"),
    ("Install", "WantedBy"),
    ("Install", "RequiredBy"),
    ("Install", "Also"),
];

/// Directives by which starting or enabling one unit also starts or enables
/// another, as `(section, key)`.
const PULL_IN_REFERENCES: &[(&str, &str)] = &[
    ("Unit", "Wants"),
    ("Unit", "Requires"),
    ("Unit", "Requisite"),
    ("Unit", "BindsTo"),
    ("Unit", "Upholds"),
    ("Install", "Also"),
];

/// The repository root: the parent of this crate's manifest directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Reads a repository file, given relative to the repository root, as UTF-8.
fn read_repo_file(relative: &str) -> TestResult<String> {
    fs::read_to_string(repo_root().join(relative))
        .map_err(|error| TestError::Unexpected(format!("read {relative}: {error}")))
}

// ---------------------------------------------------------------------------
// The packaging manifest
// ---------------------------------------------------------------------------

/// One line of `harw-dod/packaging/manifest`: `<prefix>/<name>`.
#[derive(Debug)]
struct ManifestEntry {
    /// Logical prefix, one of [`MANIFEST_PREFIXES`].
    prefix: String,
    /// File name below the prefix's directory (no `/`).
    name: String,
}

/// The parsed packaging manifest, entries in file order.
#[derive(Debug)]
struct Manifest {
    /// Every entry, in file order.
    entries: Vec<ManifestEntry>,
}

impl Manifest {
    /// Reads and parses `harw-dod/packaging/manifest` of this checkout.
    fn load() -> TestResult<Self> {
        Self::parse(&read_repo_file("harw-dod/packaging/manifest")?)
    }

    /// Parses manifest text by the rules of `install.sh uninstall`: empty
    /// lines and `#` comments are skipped, every other line is taken verbatim
    /// and must be `<known prefix>/<file name>`.
    fn parse(text: &str) -> TestResult<Self> {
        let mut entries = Vec::new();
        for line in text.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.trim() != line {
                return Err(TestError::Unexpected(format!(
                    "manifest line with surrounding whitespace: {line:?}"
                )));
            }
            let Some((prefix, name)) = line.split_once('/') else {
                return Err(TestError::Unexpected(format!(
                    "manifest line without a prefix: {line:?}"
                )));
            };
            if !MANIFEST_PREFIXES.contains(&prefix) || name.is_empty() || name.contains('/') {
                return Err(TestError::Unexpected(format!(
                    "unknown manifest entry: {line:?}"
                )));
            }
            entries.push(ManifestEntry {
                prefix: prefix.to_owned(),
                name: name.to_owned(),
            });
        }
        Ok(Self { entries })
    }

    /// The names listed under `prefix`, in manifest order.
    fn names(&self, prefix: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|entry| entry.prefix == prefix)
            .map(|entry| entry.name.as_str())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Conservative readers for install.sh and harw-dod/Makefile
// ---------------------------------------------------------------------------

/// A shell line without surrounding whitespace, or `None` for an empty or
/// comment line.
fn code_line(line: &str) -> Option<&str> {
    let line = line.trim();
    (!line.is_empty() && !line.starts_with('#')).then_some(line)
}

/// The body lines of the shell function `name`: everything between the
/// `name() {` line and the next line that is exactly `}`.
///
/// # Errors
/// [`TestError::Unexpected`] when the function is missing or not closed.
fn shell_function<'a>(script: &'a str, name: &str) -> TestResult<Vec<&'a str>> {
    let header = format!("{name}() {{");
    let mut lines = script.lines().skip_while(|line| *line != header.as_str());
    if lines.next().is_none() {
        return Err(TestError::Unexpected(format!(
            "install.sh defines no {name}()"
        )));
    }
    let mut body = Vec::new();
    for line in lines {
        if line == "}" {
            return Ok(body);
        }
        body.push(line);
    }
    Err(TestError::Unexpected(format!(
        "{name}() in install.sh is not closed"
    )))
}

/// The word lists of every `for <var> in …; do` loop in `lines`, in order.
///
/// # Errors
/// [`TestError::Unexpected`] for a `for <var>` loop written in any other
/// form, so that a reformatted loop fails loudly instead of being skipped.
fn for_loops<'a>(lines: &[&'a str], var: &str) -> TestResult<Vec<Vec<&'a str>>> {
    let head = format!("for {var} ");
    let form = format!("for {var} in ");
    let mut loops = Vec::new();
    for line in lines.iter().copied().filter_map(code_line) {
        if !line.starts_with(head.as_str()) {
            continue;
        }
        let words = line
            .strip_prefix(form.as_str())
            .and_then(|rest| rest.strip_suffix("; do"))
            .ok_or_else(|| TestError::Unexpected(format!("unparsable loop head: {line}")))?;
        loops.push(words.split_whitespace().collect());
    }
    Ok(loops)
}

/// The words of the single-quoted assignment `name='…'` at column 0.
///
/// # Errors
/// [`TestError::Unexpected`] unless the script assigns `name` exactly once.
fn quoted_words<'a>(script: &'a str, name: &str) -> TestResult<Vec<&'a str>> {
    let prefix = format!("{name}='");
    let mut values = script
        .lines()
        .filter_map(|line| line.strip_prefix(prefix.as_str())?.strip_suffix('\''));
    match (values.next(), values.next()) {
        (Some(value), None) => Ok(value.split_whitespace().collect()),
        _ => Err(TestError::Unexpected(format!(
            "install.sh must assign {name}='…' exactly once"
        ))),
    }
}

/// The word after every `systemctl` that `lines` runs; `command -v
/// systemctl` probes are not invocations and are skipped.
fn systemctl_verbs<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let mut verbs = Vec::new();
    for line in lines.iter().copied().filter_map(code_line) {
        let mut previous = "";
        let mut words = line.split_whitespace().peekable();
        while let Some(word) = words.next() {
            if word == "systemctl" && previous != "-v" {
                verbs.push(words.peek().copied().unwrap_or(""));
            }
            previous = word;
        }
    }
    verbs
}

/// The guard of the host-only block in `uninstall_package()`: an empty
/// `DESTDIR` and an available `systemctl`.
const HOST_GUARD: &str = "if [ -z \"$destdir\" ] && command -v systemctl >/dev/null 2>&1; then";

/// The word list of the uninstall disable loop: the checked `dod_units`.
const DOD_UNITS_WORD: &str = "$dod_units";

/// The head of the uninstall disable loop.
const DISABLE_LOOP: &str = "for unit in $dod_units; do";

/// The one `systemctl disable` call of `uninstall_package()`, per unit.
const DISABLE_CALL: &str = "systemctl disable --now \"$unit\" >/dev/null 2>&1 || true";

/// The head of the manifest-driven removal loop of `uninstall_package()`.
const REMOVAL_LOOP: &str = "while IFS= read -r entry";

/// The units that `uninstall_package()` stops and disables on a host.
///
/// Conservative reader: the function must run `systemctl disable` exactly
/// once, as [`DISABLE_CALL`] in the body of [`DISABLE_LOOP`], directly
/// under [`HOST_GUARD`] (a host install only), before the manifest loop
/// ([`REMOVAL_LOOP`]) removes the unit files.
///
/// # Returns
/// The words of `dod_units`, the list the loop iterates over.
///
/// # Errors
/// [`TestError::Unexpected`] for any other form (a list of its own, a
/// batch call, no guard, after the removal), and when `dod_units` is not
/// assigned exactly once.
fn uninstall_disabled_units(script: &str) -> TestResult<Vec<&str>> {
    let body = shell_function(script, "uninstall_package")?;
    let code: Vec<&str> = body.iter().copied().filter_map(code_line).collect();
    let calls = code
        .iter()
        .filter(|line| line.contains("systemctl disable"))
        .count();
    if calls != 1 {
        return Err(TestError::Unexpected(format!(
            "uninstall_package() must run `systemctl disable` exactly once (per unit, in a loop), found {calls}"
        )));
    }
    let block = code
        .windows(4)
        .position(|window| *window == [HOST_GUARD, DISABLE_LOOP, DISABLE_CALL, "done"])
        .ok_or_else(|| {
            TestError::Unexpected(format!(
                "uninstall_package() must stop and disable units as `{HOST_GUARD}` / `{DISABLE_LOOP}` / `{DISABLE_CALL}` / `done`"
            ))
        })?;
    let removal = code
        .iter()
        .position(|line| line.starts_with(REMOVAL_LOOP))
        .ok_or(TestError::Missing(
            "the manifest loop of uninstall_package()",
        ))?;
    if block > removal {
        return Err(TestError::Unexpected(
            "uninstall_package() disables the units only after removing their files".to_owned(),
        ));
    }
    quoted_words(script, "dod_units")
}

/// Whether a shell `case` pattern (`a/*`, `a/b`, alternatives with `|`)
/// matches `path`.
fn case_matches(pattern: &str, path: &str) -> bool {
    pattern
        .split('|')
        .any(|alternative| match alternative.strip_suffix('*') {
            Some(prefix) => path.starts_with(prefix),
            None => alternative == path,
        })
}

/// The recipe lines (tab-indented) of the make target `target`.
///
/// # Errors
/// [`TestError::Unexpected`] when the makefile has no such target.
fn make_recipe<'a>(makefile: &'a str, target: &str) -> TestResult<Vec<&'a str>> {
    let mut lines = makefile
        .lines()
        .skip_while(|line| line.split_once(':').map(|(name, _)| name) != Some(target));
    if lines.next().is_none() {
        return Err(TestError::Unexpected(format!(
            "harw-dod/Makefile has no target {target}"
        )));
    }
    Ok(lines.take_while(|line| line.starts_with('\t')).collect())
}

/// Whether a shell or make line runs `systemctl enable` for a unit whose
/// name contains `unit_stem`. Lines that only print a message (`printf`,
/// `echo`) are not invocations.
fn enables(line: &str, unit_stem: &str) -> bool {
    let words: Vec<&str> = line.split_whitespace().collect();
    if words
        .first()
        .is_some_and(|word| matches!(*word, "printf" | "echo" | "@echo"))
    {
        return false;
    }
    let runs_enable = words.iter().zip(words.iter().skip(1)).any(|(tool, verb)| {
        (tool.ends_with("systemctl") || tool.ends_with("systemctl_bin\"")) && *verb == "enable"
    });
    runs_enable && words.iter().any(|word| word.contains(unit_stem))
}

/// Repository-relative paths of the DoD packaging entry points:
/// `harw-dod/Makefile` and every `harw-dod/scripts/*.sh`, sorted.
fn packaging_entry_points() -> TestResult<Vec<String>> {
    let mut scripts = Vec::new();
    for entry in
        fs::read_dir(repo_root().join("harw-dod/scripts")).map_err(ctx("read harw-dod/scripts"))?
    {
        let entry = entry.map_err(ctx("read a harw-dod/scripts entry"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".sh") {
            scripts.push(format!("harw-dod/scripts/{name}"));
        }
    }
    scripts.sort();
    let mut paths = vec!["harw-dod/Makefile".to_owned()];
    paths.extend(scripts);
    Ok(paths)
}

// ---------------------------------------------------------------------------
// Units
// ---------------------------------------------------------------------------

/// A unit as `install.sh` installs it: read from `deploy/systemd/` and
/// rendered with the installer defaults ([`RenderPaths::default`]).
fn rendered_deploy_unit(name: &str) -> TestResult<ParsedUnit> {
    let text = read_repo_file(&format!("deploy/systemd/{name}"))?;
    Ok(parse_unit(&render(&text, &RenderPaths::default())))
}

/// The program of an `Exec*=` value: the first word with the systemd prefix
/// characters (`@ - : + !`) stripped.
fn exec_program(value: &str) -> Option<&str> {
    value
        .split_whitespace()
        .next()
        .map(|word| word.trim_start_matches(['@', '-', ':', '+', '!']))
}

/// The binary name of a rendered program path that lies directly in the
/// DoD libexec directory (`@LIBEXECDIR@`), or `None` for any other path.
fn libexec_binary<'a>(program: &'a str, paths: &RenderPaths) -> Option<&'a str> {
    program
        .strip_prefix(paths.libexecdir.as_str())?
        .strip_prefix('/')
        .filter(|name| !name.is_empty() && !name.contains('/'))
}

/// The unit names of space-separated directive values.
fn unit_words<'a>(values: &[&'a str]) -> Vec<&'a str> {
    values
        .iter()
        .copied()
        .flat_map(str::split_whitespace)
        .collect()
}

/// The DoD binary that the rendered `key=value` line of `unit_name` runs
/// from the libexec directory.
///
/// # Returns
/// `Some(binary)` for a program in the libexec directory; `None` for a
/// helper outside it (a system tool in `ExecStartPre=` and similar).
///
/// # Errors
/// [`TestError::Unexpected`] for an empty value, and for an `ExecStart=`
/// outside the libexec directory: the main program of a DoD unit is always
/// one the package installs.
fn packaged_program<'a>(
    unit_name: &str,
    key: &str,
    value: &'a str,
    paths: &RenderPaths,
) -> TestResult<Option<&'a str>> {
    let program = exec_program(value)
        .ok_or_else(|| TestError::Unexpected(format!("{unit_name}: empty {key}=")))?;
    match libexec_binary(program, paths) {
        Some(binary) => Ok(Some(binary)),
        None if key == "ExecStart" => Err(TestError::Unexpected(format!(
            "{unit_name}: ExecStart= runs {program}, outside {} where the DoD package installs its programs",
            paths.libexecdir
        ))),
        None => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Static contract
// ---------------------------------------------------------------------------

#[test]
fn test_manifest_lists_the_dod_package_in_order() -> TestResult {
    let manifest = Manifest::load()?;
    assert_eq!(
        manifest.names("libexec"),
        DOD_PACKAGE_BINARIES,
        "manifest libexec/ entries differ from DOD_PACKAGE_BINARIES (order included)"
    );
    for (prefix, deploy_dir) in [
        ("systemd", "deploy/systemd/"),
        ("sysusers", "deploy/sysusers.d/"),
        ("tmpfiles", "deploy/tmpfiles.d/"),
    ] {
        let package: Vec<&str> = DOD_PACKAGE_ASSETS
            .iter()
            .copied()
            .filter_map(|path| path.strip_prefix(deploy_dir))
            .collect();
        assert_eq!(
            manifest.names(prefix),
            package,
            "manifest {prefix}/ entries differ from DOD_PACKAGE_ASSETS (order included)"
        );
    }
    let unique: BTreeSet<(&str, &str)> = manifest
        .entries
        .iter()
        .map(|entry| (entry.prefix.as_str(), entry.name.as_str()))
        .collect();
    assert_eq!(
        unique.len(),
        manifest.entries.len(),
        "the manifest lists an entry twice"
    );
    Ok(())
}

#[test]
fn test_install_script_repeats_the_manifest_lists_in_order() -> TestResult {
    let manifest = Manifest::load()?;
    let script = read_repo_file("harw-dod/scripts/install.sh")?;
    assert_eq!(
        quoted_words(&script, "dod_units")?,
        manifest.names("systemd"),
        "install.sh dod_units differs from the manifest systemd/ entries (order included)"
    );
    for function in ["check_artifacts", "install_package"] {
        let body = shell_function(&script, function)?;
        assert_eq!(
            for_loops(&body, "binary")?,
            [manifest.names("libexec")],
            "install.sh {function}(): the binary loop differs from the manifest libexec/ entries"
        );
        assert_eq!(
            for_loops(&body, "object")?,
            [manifest.names("bpf")],
            "install.sh {function}(): the object loop differs from the manifest bpf/ entries"
        );
    }
    // No third, unchecked copy of either list anywhere else in the script.
    let lines: Vec<&str> = script.lines().collect();
    assert_eq!(
        for_loops(&lines, "binary")?.len(),
        2,
        "install.sh loops over the binaries outside check_artifacts()/install_package()"
    );
    assert_eq!(
        for_loops(&lines, "object")?.len(),
        2,
        "install.sh loops over the BPF objects outside check_artifacts()/install_package()"
    );
    Ok(())
}

#[test]
fn test_install_script_never_enables_or_starts_a_unit() -> TestResult {
    let script = read_repo_file("harw-dod/scripts/install.sh")?;
    let install = shell_function(&script, "install_package")?;
    let install_verbs = systemctl_verbs(&install);
    assert!(
        install_verbs.contains(&"daemon-reload"),
        "install_package() reloads systemd after installing units: {install_verbs:?}"
    );
    assert!(
        install_verbs.iter().all(|verb| *verb == "daemon-reload"),
        "install_package() may only run `systemctl daemon-reload`: {install_verbs:?}"
    );
    let lines: Vec<&str> = script.lines().collect();
    for verb in systemctl_verbs(&lines) {
        assert!(
            matches!(verb, "daemon-reload" | "disable"),
            "install.sh runs `systemctl {verb}`; it must never enable or start a unit"
        );
    }
    Ok(())
}

#[test]
fn test_uninstall_is_manifest_driven_and_stops_every_shipped_unit() -> TestResult {
    let manifest = Manifest::load()?;
    let script = read_repo_file("harw-dod/scripts/install.sh")?;
    let body = shell_function(&script, "uninstall_package")?;
    let code: Vec<&str> = body.iter().copied().filter_map(code_line).collect();

    assert!(
        code.iter().any(|line| line.starts_with(REMOVAL_LOOP)),
        "uninstall_package() must remove files in a `{REMOVAL_LOOP}` loop"
    );
    assert!(
        code.iter()
            .any(|line| line.starts_with("done < \"$manifest\"")),
        "uninstall_package() must read its file list from the packaging manifest"
    );
    for var in ["binary", "object"] {
        assert!(
            for_loops(&body, var)?.is_empty(),
            "uninstall_package() keeps its own {var} list; the manifest must be the only list"
        );
    }
    assert_eq!(
        for_loops(&body, "unit")?,
        [[DOD_UNITS_WORD]],
        "uninstall_package() may loop over units only through the checked dod_units list"
    );
    let arms: Vec<&str> = code
        .iter()
        .filter_map(|line| line.split_once(") destination="))
        .map(|(pattern, _)| pattern)
        .collect();
    for entry in &manifest.entries {
        let path = format!("{}/{}", entry.prefix, entry.name);
        assert!(
            arms.iter().any(|pattern| case_matches(pattern, &path)),
            "uninstall_package() has no case arm for manifest entry {path}: {arms:?}"
        );
    }

    // On a host, every shipped unit (the target, the opt-in probes and the
    // Warden included) is stopped and disabled before its file goes away, so
    // no `.wants` symlink dangles and nothing keeps running from a deleted
    // binary. The reader also checks the DESTDIR guard and the order.
    let disabled = uninstall_disabled_units(&script)?;
    assert_eq!(
        disabled,
        manifest.names("systemd"),
        "uninstall_package() must stop and disable exactly the shipped units"
    );
    for unit in ["harw-dod.target", "harw-probe-fs.service", NETLINK_UNIT] {
        assert!(
            disabled.contains(&unit),
            "uninstall_package() must stop and disable {unit}: {disabled:?}"
        );
    }
    Ok(())
}

#[test]
fn test_every_shipped_unit_runs_a_shipped_binary() -> TestResult {
    let manifest = Manifest::load()?;
    let shipped_binaries = manifest.names("libexec");
    let shipped_units = manifest.names("systemd");
    let paths = RenderPaths::default();
    let mut started = BTreeSet::new();
    for unit_name in shipped_units.iter().copied() {
        let unit = rendered_deploy_unit(unit_name)?;
        if unit_name.ends_with(".service") {
            assert!(
                unit.last_value("Service", "ExecStart").is_some(),
                "{unit_name}: no ExecStart="
            );
            for key in EXEC_KEYS.iter().copied() {
                for value in unit.values("Service", key) {
                    let Some(binary) = packaged_program(unit_name, key, value, &paths)? else {
                        continue;
                    };
                    assert!(
                        shipped_binaries.contains(&binary),
                        "{unit_name}: {key}= runs {binary}, which the manifest does not ship under libexec/"
                    );
                    started.insert(binary.to_owned());
                }
            }
        }
        if unit_name.ends_with(".socket") {
            let service = unit
                .last_value("Socket", "Service")
                .map_or_else(|| unit_name.replace(".socket", ".service"), str::to_owned);
            assert!(
                shipped_units.contains(&service.as_str()),
                "{unit_name} activates {service}, which the manifest does not ship"
            );
        }
        for (section, key) in UNIT_REFERENCES.iter().copied() {
            for referenced in unit_words(&unit.values(section, key)) {
                assert!(
                    !referenced.starts_with("harw-") || shipped_units.contains(&referenced),
                    "{unit_name}: {key}={referenced} names a harw unit the DoD package does not ship"
                );
            }
        }
    }
    for class in UNIT_CLASSES {
        assert!(
            started.contains(class.binary),
            "no shipped unit starts {}",
            class.binary
        );
    }
    Ok(())
}

#[test]
fn test_every_shipped_binary_ships_its_unit() -> TestResult {
    let manifest = Manifest::load()?;
    let shipped_units = manifest.names("systemd");
    let paths = RenderPaths::default();
    let unit_dir = repo_root().join("deploy/systemd");
    for binary in manifest.names("libexec") {
        let service = format!("{binary}.service");
        if !unit_dir.join(&service).is_file() {
            continue;
        }
        assert!(
            shipped_units.contains(&service.as_str()),
            "{binary} ships, but its unit deploy/systemd/{service} does not"
        );
        let unit = rendered_deploy_unit(&service)?;
        let runs: Vec<&str> = unit
            .values("Service", "ExecStart")
            .into_iter()
            .filter_map(exec_program)
            .filter_map(|program| libexec_binary(program, &paths))
            .collect();
        assert!(
            runs.contains(&binary),
            "{service} does not start {binary}: {runs:?}"
        );
    }

    let classes: BTreeSet<&str> = UNIT_CLASSES.iter().map(|class| class.binary).collect();
    let package: BTreeSet<&str> = DOD_PACKAGE_BINARIES.iter().copied().collect();
    assert_eq!(
        classes, package,
        "UNIT_CLASSES and DOD_PACKAGE_BINARIES name different binaries"
    );
    for class in UNIT_CLASSES {
        assert!(
            shipped_units.contains(&class.service_file),
            "{} is a DoD unit class, but the manifest does not ship it",
            class.service_file
        );
    }
    Ok(())
}

#[test]
fn test_dod_build_recipe_builds_every_shipped_binary() -> TestResult {
    let makefile = read_repo_file("harw-dod/Makefile")?;
    let recipe = make_recipe(&makefile, "build")?;
    let words: Vec<&str> = recipe
        .iter()
        .copied()
        .flat_map(str::split_whitespace)
        .map(|word| word.trim_end_matches(';'))
        .collect();
    for binary in DOD_PACKAGE_BINARIES {
        assert!(
            words
                .iter()
                .zip(words.iter().skip(1))
                .any(|(flag, package)| *flag == "-p" && package == binary),
            "harw-dod/Makefile `build` does not build {binary}; `make install` would stop in check_artifacts"
        );
    }
    Ok(())
}

#[test]
fn test_netlink_probe_ships_everywhere_but_stays_opt_in() -> TestResult {
    let manifest = Manifest::load()?;
    let script = read_repo_file("harw-dod/scripts/install.sh")?;

    // Shipped everywhere.
    assert!(DOD_PACKAGE_BINARIES.contains(&NETLINK_PROBE));
    assert!(DOD_PACKAGE_ASSETS.contains(&"deploy/systemd/harw-probe-netlink.service"));
    assert!(
        UNIT_CLASSES
            .iter()
            .any(|class| class.binary == NETLINK_PROBE && class.service_file == NETLINK_UNIT)
    );
    assert!(manifest.names("libexec").contains(&NETLINK_PROBE));
    assert!(manifest.names("systemd").contains(&NETLINK_UNIT));
    assert!(quoted_words(&script, "dod_units")?.contains(&NETLINK_UNIT));
    for function in ["check_artifacts", "install_package"] {
        let loops = for_loops(&shell_function(&script, function)?, "binary")?;
        assert!(
            !loops.is_empty() && loops.iter().all(|list| list.contains(&NETLINK_PROBE)),
            "install.sh {function}() does not handle {NETLINK_PROBE}: {loops:?}"
        );
    }
    assert!(
        uninstall_disabled_units(&script)?.contains(&NETLINK_UNIT),
        "install.sh uninstall does not stop and disable {NETLINK_UNIT}"
    );

    // But never pulled in: neither by the observation target nor by any other
    // shipped unit.
    let target = rendered_deploy_unit("harw-dod.target")?;
    let wants = unit_words(&target.values("Unit", "Wants"));
    assert!(
        !wants.contains(&NETLINK_UNIT),
        "harw-dod.target Wants={wants:?} pulls in the opt-in netlink probe"
    );
    for unit_name in manifest.names("systemd") {
        if unit_name == NETLINK_UNIT {
            continue;
        }
        let unit = rendered_deploy_unit(unit_name)?;
        for (section, key) in PULL_IN_REFERENCES.iter().copied() {
            assert!(
                !unit_words(&unit.values(section, key)).contains(&NETLINK_UNIT),
                "{unit_name}: {key}= pulls in the opt-in {NETLINK_UNIT}"
            );
        }
    }

    // And never enabled by a packaging entry point (make install/stage,
    // service.sh enable, install.sh).
    for relative in packaging_entry_points()? {
        let text = read_repo_file(&relative)?;
        for line in text.lines().filter_map(code_line) {
            assert!(
                !enables(line, NETLINK_PROBE),
                "{relative} enables the opt-in netlink probe: {line}"
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The readers themselves
// ---------------------------------------------------------------------------

#[test]
fn test_manifest_parser_follows_the_uninstall_rules() -> TestResult {
    let manifest = Manifest::parse("# comment\n\nlibexec/a\nsystemd/a.service\nlibexec/b\n")?;
    assert_eq!(manifest.names("libexec"), ["a", "b"]);
    assert_eq!(manifest.names("systemd"), ["a.service"]);
    assert!(manifest.names("bpf").is_empty());
    for bad in [
        "bogus/a",
        " libexec/a",
        "libexec/a ",
        "libexec/",
        "libexec/a/b",
        "libexec",
    ] {
        assert!(Manifest::parse(bad).is_err(), "accepted {bad:?}");
    }
    Ok(())
}

#[test]
fn test_shell_readers_parse_functions_loops_and_assignments() -> TestResult {
    let script = "list='a  b'\n\
                  good() {\n\
                  \tfor binary in a b; do\n\
                  \t\t# for binary in commented; do\n\
                  \tdone\n\
                  }\n\
                  bad() {\n\
                  \tfor binary in $list\n\
                  \tdo\n\
                  }\n\
                  open() {\n";
    assert_eq!(quoted_words(script, "list")?, ["a", "b"]);
    assert!(quoted_words(script, "missing").is_err());
    assert!(quoted_words("x='a'\nx='b'\n", "x").is_err());

    let good = shell_function(script, "good")?;
    assert_eq!(good.len(), 3);
    assert_eq!(for_loops(&good, "binary")?, [["a", "b"]]);
    assert!(for_loops(&good, "object")?.is_empty());
    let bad = shell_function(script, "bad")?;
    assert!(
        for_loops(&bad, "binary").is_err(),
        "a reformatted loop must fail, not be skipped"
    );
    assert!(shell_function(script, "missing").is_err());
    assert!(shell_function(script, "open").is_err());
    Ok(())
}

#[test]
fn test_systemctl_readers_find_invocations_only() -> TestResult {
    let lines = [
        "command -v systemctl >/dev/null 2>&1 && systemctl daemon-reload || true",
        "\tsystemctl enable --now x.service",
        "fail 'systemctl is required'",
        "# systemctl start y.service",
    ];
    assert_eq!(systemctl_verbs(&lines), ["daemon-reload", "enable"]);
    Ok(())
}

#[test]
fn test_uninstall_disable_reader_accepts_only_the_guarded_per_unit_loop() -> TestResult {
    let good = "dod_units='a.target b.service'\n\
                uninstall_package() {\n\
                \t# systemctl disable in a comment is not a call\n\
                \tif [ -z \"$destdir\" ] && command -v systemctl >/dev/null 2>&1; then\n\
                \t\tfor unit in $dod_units; do\n\
                \t\t\tsystemctl disable --now \"$unit\" >/dev/null 2>&1 || true\n\
                \t\tdone\n\
                \tfi\n\
                \twhile IFS= read -r entry || [ -n \"$entry\" ]; do\n\
                \t\trm -f \"$entry\"\n\
                \tdone < \"$manifest\"\n\
                }\n";
    assert_eq!(uninstall_disabled_units(good)?, ["a.target", "b.service"]);

    let unguarded = good.replace(
        "\tif [ -z \"$destdir\" ] && command -v systemctl >/dev/null 2>&1; then\n",
        "",
    );
    let own_list = good.replace(
        "for unit in $dod_units; do",
        "for unit in a.target b.service; do",
    );
    let batch = good.replace(
        "\t\tfor unit in $dod_units; do\n\
         \t\t\tsystemctl disable --now \"$unit\" >/dev/null 2>&1 || true\n\
         \t\tdone\n",
        "\t\tsystemctl disable --now a.target b.service >/dev/null 2>&1 || true\n",
    );
    let twice = good.replace(
        "\tfi\n",
        "\tfi\n\tsystemctl disable --now c.service >/dev/null 2>&1 || true\n",
    );
    let after_removal = "dod_units='a.target'\n\
                         uninstall_package() {\n\
                         \twhile IFS= read -r entry || [ -n \"$entry\" ]; do\n\
                         \t\trm -f \"$entry\"\n\
                         \tdone < \"$manifest\"\n\
                         \tif [ -z \"$destdir\" ] && command -v systemctl >/dev/null 2>&1; then\n\
                         \t\tfor unit in $dod_units; do\n\
                         \t\t\tsystemctl disable --now \"$unit\" >/dev/null 2>&1 || true\n\
                         \t\tdone\n\
                         \tfi\n\
                         }\n";
    let no_removal = good.replace("\twhile IFS= read -r entry", "\tuntil false");
    for (name, script) in [
        ("unguarded", unguarded.as_str()),
        ("own list", own_list.as_str()),
        ("batch", batch.as_str()),
        ("twice", twice.as_str()),
        ("after removal", after_removal),
        ("no removal loop", no_removal.as_str()),
    ] {
        assert_ne!(
            script, good,
            "{name}: the variant did not change the script"
        );
        assert!(
            uninstall_disabled_units(script).is_err(),
            "accepted the {name} variant"
        );
    }
    Ok(())
}

#[test]
fn test_case_make_and_enable_readers() -> TestResult {
    assert!(case_matches("libexec/*", "libexec/harw-x"));
    assert!(!case_matches("libexec/*", "bpf/x.o"));
    assert!(case_matches(
        "config/config.toml.example",
        "config/config.toml.example"
    ));
    assert!(!case_matches("config/config.toml.example", "config/other"));
    assert!(case_matches("a/*|b/*", "b/c"));

    let makefile = "build-bpf: x\n\tnot this\nbuild:\n\t@cargo build \\\n\t\t-p one -p two; \\\n\t$(MAKE) x\n\ncheck:\n\t-p three\n";
    assert_eq!(
        make_recipe(makefile, "build")?,
        ["\t@cargo build \\", "\t\t-p one -p two; \\", "\t$(MAKE) x"]
    );
    assert!(make_recipe(makefile, "missing").is_err());

    assert!(enables(
        "\"$systemctl_bin\" enable --now harw-probe-netlink.service",
        "harw-probe-netlink"
    ));
    assert!(enables(
        "\t$(PRIV_ESC) systemctl enable harw-probe-netlink.service; \\",
        "harw-probe-netlink"
    ));
    assert!(!enables(
        "systemctl disable --now harw-probe-netlink.service",
        "harw-probe-netlink"
    ));
    assert!(!enables(
        "systemctl enable --now harw-dod.target",
        "harw-probe-netlink"
    ));
    assert!(!enables(
        "printf '%s\\n' 'systemctl enable harw-probe-netlink.service'",
        "harw-probe-netlink"
    ));
    Ok(())
}

#[test]
fn test_exec_readers_resolve_only_the_libexec_directory() -> TestResult {
    let paths = RenderPaths::default();
    assert_eq!(
        exec_program("-/usr/local/libexec/harw-dod/harw-x --flag"),
        Some("/usr/local/libexec/harw-dod/harw-x")
    );
    assert_eq!(exec_program("   "), None);
    assert_eq!(
        libexec_binary("/usr/local/libexec/harw-dod/harw-x", &paths),
        Some("harw-x")
    );
    assert_eq!(libexec_binary("/usr/bin/harw", &paths), None);
    assert_eq!(
        libexec_binary("/usr/local/libexec/harw-dod/sub/harw-x", &paths),
        None
    );
    assert_eq!(libexec_binary("/usr/local/libexec/harw-dod/", &paths), None);
    assert_eq!(
        libexec_binary("/usr/local/libexec/harw-dodx/harw-x", &paths),
        None
    );

    assert_eq!(
        packaged_program(
            "u",
            "ExecStart",
            "/usr/local/libexec/harw-dod/harw-x --a",
            &paths
        )?,
        Some("harw-x")
    );
    assert_eq!(
        packaged_program("u", "ExecStartPre", "/bin/mkdir -p /run/x", &paths)?,
        None
    );
    assert!(packaged_program("u", "ExecStart", "/usr/bin/harw web", &paths).is_err());
    assert!(packaged_program("u", "ExecStop", "   ", &paths).is_err());
    Ok(())
}

// ---------------------------------------------------------------------------
// Staged install (runs harw-dod/scripts/install.sh into a DESTDIR)
// ---------------------------------------------------------------------------

/// Runs `harw-dod/scripts/install.sh install|uninstall` with `DESTDIR` set
/// and checks the staged tree. Linux only: the DoD package is a systemd
/// package for Linux hosts.
#[cfg(target_os = "linux")]
mod staged {
    use std::collections::BTreeSet;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use harw_install::deployment::{asset, render, unresolved_placeholders};
    use harw_install::dod_units::parse_unit;
    use harw_install::{DOD_PACKAGE_BINARIES, RenderPaths, UNIT_CLASSES};

    use super::{
        EXEC_KEYS, Manifest, ManifestEntry, NETLINK_PROBE, NETLINK_UNIT, TestError, TestResult,
        ctx, packaged_program, read_repo_file, repo_root,
    };

    /// `install.sh`'s `$prefix` default.
    const PREFIX: &str = "/usr/local";
    /// `install.sh`'s `$libdir` default (`$prefix/lib/harw-dod`).
    const LIB_DIR: &str = "/usr/local/lib/harw-dod";
    /// `install.sh`'s `$LOCALSTATEDIR` default.
    const LOCALSTATE_DIR: &str = "/var";
    /// `install.sh`'s `$unitdir` default (`$prefix/lib/systemd/system`).
    const UNIT_DIR: &str = "/usr/local/lib/systemd/system";
    /// `install.sh`'s `$sysusersdir` default (`$prefix/lib/sysusers.d`).
    const SYSUSERS_DIR: &str = "/usr/local/lib/sysusers.d";
    /// `install.sh`'s `$tmpfilesdir` default (`$prefix/lib/tmpfiles.d`).
    const TMPFILES_DIR: &str = "/usr/local/lib/tmpfiles.d";

    /// The only real programs on `PATH` for the host-path uninstall run:
    /// what `install.sh uninstall` needs besides shell builtins and the
    /// fakes ([`FAKE_ID`], [`FAKE_SYSTEMCTL`]).
    const HOST_UNINSTALL_TOOLS: &[&str] = &["dirname", "rm"];

    /// Stand-in `id`: reports uid 0, so `require_root_for_host_install`
    /// passes without root.
    const FAKE_ID: &str = "#!/bin/sh\nprintf '%s\\n' 0\n";

    /// Stand-in `systemctl`: appends `<arguments> present|absent` to
    /// `$HARW_TEST_SYSTEMCTL_LOG`, where the state says whether the unit
    /// file named by the last argument still exists in `$SYSTEMD_UNITDIR`.
    const FAKE_SYSTEMCTL: &str = "#!/bin/sh\n\
        unit=\n\
        for arg in \"$@\"; do unit=$arg; done\n\
        if [ -e \"$SYSTEMD_UNITDIR/$unit\" ]; then state=present; else state=absent; fi\n\
        printf '%s %s\\n' \"$*\" \"$state\" >> \"$HARW_TEST_SYSTEMCTL_LOG\"\n";

    /// The upper-case variables a shell script reads with a default
    /// (`${NAME:-…}`), i.e. the environment it can be redirected by.
    fn defaulted_variables(script: &str) -> BTreeSet<&str> {
        script
            .split("${")
            .skip(1)
            .filter_map(|rest| {
                let (name, _) = rest.split_once(":-")?;
                (!name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                    && name.starts_with(|c: char| c.is_ascii_uppercase()))
                .then_some(name)
            })
            .collect()
    }

    /// The first regular file `name` on the caller's `PATH`.
    ///
    /// # Errors
    /// [`TestError::Missing`] without `PATH`, [`TestError::Unexpected`]
    /// when no directory of it holds `name`.
    fn find_tool(name: &str) -> TestResult<PathBuf> {
        let path = std::env::var_os("PATH").ok_or(TestError::Missing("PATH"))?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_absolute() && candidate.is_file())
            .ok_or_else(|| TestError::Unexpected(format!("{name} is not on PATH")))
    }

    /// Every variable `install.sh` reads; removed from the child environment
    /// so the caller's shell cannot redirect the staged install.
    const INSTALLER_ENV: &[&str] = &[
        "PREFIX",
        "DESTDIR",
        "LIBEXECDIR",
        "LIBDIR",
        "BPFDIR",
        "SYSCONFDIR",
        "STATEDIR",
        "LOCALSTATEDIR",
        "LOGDIR",
        "SYSTEMD_UNITDIR",
        "SYSUSERSDIR",
        "TMPFILESDIR",
        "BINARY_DIR",
        "BPF_ARTIFACT_DIR",
        "DEPLOY_DIR",
    ];

    /// A temporary stage: stand-in build artifacts and the `DESTDIR` tree.
    struct Stage {
        /// The temporary directory holding everything below.
        root: PathBuf,
        /// `DESTDIR`.
        destdir: PathBuf,
        /// `BINARY_DIR`: one executable stand-in per shipped binary.
        binaries: PathBuf,
        /// `BPF_ARTIFACT_DIR`: one stand-in per manifest `bpf/` entry.
        bpf: PathBuf,
    }

    impl Stage {
        /// Creates the stage and its stand-in artifacts.
        fn create(manifest: &Manifest) -> TestResult<Self> {
            let root = harw_test_support::unique_tmp_created("harw-install", "dod-stage")
                .map_err(ctx("create the stage directory"))?;
            let stage = Self {
                destdir: root.join("destdir"),
                binaries: root.join("artifacts/bin"),
                bpf: root.join("artifacts/bpf"),
                root,
            };
            fs::create_dir_all(&stage.binaries).map_err(ctx("create the binary artifact dir"))?;
            fs::create_dir_all(&stage.bpf).map_err(ctx("create the BPF artifact dir"))?;
            for binary in DOD_PACKAGE_BINARIES.iter().copied() {
                let path = stage.binaries.join(binary);
                fs::write(&path, "#!/bin/sh\nexit 0\n").map_err(ctx("write a stand-in binary"))?;
                fs::set_permissions(path, fs::Permissions::from_mode(0o755))
                    .map_err(ctx("make a stand-in binary executable"))?;
            }
            for object in manifest.names("bpf") {
                fs::write(stage.bpf.join(object), "stand-in BPF artifact\n")
                    .map_err(ctx("write a stand-in BPF artifact"))?;
            }
            Ok(stage)
        }

        /// Runs `install.sh <action>` against this stage.
        fn run(&self, action: &str) -> TestResult {
            // An empty DESTDIR would turn this into a host install.
            if !self.destdir.is_absolute() {
                return Err(TestError::Unexpected(format!(
                    "refusing a relative DESTDIR: {}",
                    self.destdir.display()
                )));
            }
            let script = fs::canonicalize(repo_root().join("harw-dod/scripts/install.sh"))
                .map_err(ctx("resolve harw-dod/scripts/install.sh"))?;
            let mut command = Command::new("sh");
            command.arg(script).arg(action);
            for name in INSTALLER_ENV.iter().copied() {
                command.env_remove(name);
            }
            command
                .env("DESTDIR", &self.destdir)
                .env("BINARY_DIR", &self.binaries)
                .env("BPF_ARTIFACT_DIR", &self.bpf);
            let output = command
                .output()
                .map_err(ctx("run harw-dod/scripts/install.sh"))?;
            if output.status.success() {
                Ok(())
            } else {
                Err(TestError::Unexpected(format!(
                    "install.sh {action} exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                )))
            }
        }

        /// Runs `install.sh uninstall` down its host path (empty `DESTDIR`)
        /// against the files [`Stage::run`]`("install")` staged: every
        /// directory variable points into the stage and `PATH` is `bin`
        /// alone (see [`HOST_UNINSTALL_TOOLS`]); the fake `systemctl` logs
        /// to `log`.
        fn run_host_uninstall(&self, bin: &Path, log: &Path, paths: &RenderPaths) -> TestResult {
            // Every directory must lie inside the stage, or the host path
            // would remove files of the build host.
            if !self.destdir.is_absolute() || !bin.is_absolute() {
                return Err(TestError::Unexpected(format!(
                    "refusing a relative stage: {}",
                    self.destdir.display()
                )));
            }
            // Every installer variable except DESTDIR (empty: the host path)
            // and DEPLOY_DIR (the repository's read-only deploy/ tree).
            let stage_vars = [
                ("PREFIX", self.path(PREFIX)),
                ("LIBEXECDIR", self.path(&paths.libexecdir)),
                ("LIBDIR", self.path(LIB_DIR)),
                ("BPFDIR", self.path(&paths.bpfdir)),
                ("SYSCONFDIR", self.path(&paths.sysconfdir)),
                ("STATEDIR", self.path(&paths.statedir)),
                ("LOCALSTATEDIR", self.path(LOCALSTATE_DIR)),
                ("LOGDIR", self.path(&paths.logdir)),
                ("SYSTEMD_UNITDIR", self.path(UNIT_DIR)),
                ("SYSUSERSDIR", self.path(SYSUSERS_DIR)),
                ("TMPFILESDIR", self.path(TMPFILES_DIR)),
                ("BINARY_DIR", self.binaries.clone()),
                ("BPF_ARTIFACT_DIR", self.bpf.clone()),
            ];
            for name in INSTALLER_ENV.iter().copied() {
                let redirected = matches!(name, "DESTDIR" | "DEPLOY_DIR")
                    || stage_vars.iter().any(|(set, _)| *set == name);
                if !redirected {
                    return Err(TestError::Unexpected(format!(
                        "the host-path uninstall run does not redirect {name} into the stage"
                    )));
                }
            }
            let script = fs::canonicalize(repo_root().join("harw-dod/scripts/install.sh"))
                .map_err(ctx("resolve harw-dod/scripts/install.sh"))?;
            let mut command = Command::new(find_tool("sh")?);
            command.arg(script).arg("uninstall");
            for name in INSTALLER_ENV.iter().copied() {
                command.env_remove(name);
            }
            command
                .env("PATH", bin)
                .env("HARW_TEST_SYSTEMCTL_LOG", log)
                .envs(stage_vars);
            let output = command
                .output()
                .map_err(ctx("run harw-dod/scripts/install.sh uninstall"))?;
            if output.status.success() {
                Ok(())
            } else {
                Err(TestError::Unexpected(format!(
                    "host-path install.sh uninstall exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                )))
            }
        }

        /// The staged location of an absolute logical path.
        fn path(&self, logical: &str) -> PathBuf {
            self.destdir.join(logical.trim_start_matches('/'))
        }

        /// Every non-directory entry below `DESTDIR`, as an absolute logical
        /// path (`/usr/local/…`).
        fn files(&self) -> TestResult<BTreeSet<String>> {
            let mut files = BTreeSet::new();
            let mut pending = vec![self.destdir.clone()];
            while let Some(dir) = pending.pop() {
                for entry in fs::read_dir(dir).map_err(ctx("read a staged directory"))? {
                    let entry = entry.map_err(ctx("read a staged directory entry"))?;
                    let path = entry.path();
                    if entry
                        .file_type()
                        .map_err(ctx("stat a staged entry"))?
                        .is_dir()
                    {
                        pending.push(path);
                        continue;
                    }
                    let relative = path
                        .strip_prefix(&self.destdir)
                        .map_err(ctx("strip the DESTDIR prefix"))?;
                    let mut logical = String::new();
                    for component in relative.components() {
                        logical.push('/');
                        logical.push_str(&component.as_os_str().to_string_lossy());
                    }
                    files.insert(logical);
                }
            }
            Ok(files)
        }
    }

    impl Drop for Stage {
        /// Deletes the stage with everything below it, also when a check
        /// failed: an `assert!` unwinds through this drop and a `?` error
        /// drops the stage on return, so no fake `/usr/local` tree is left
        /// under the temporary directory.
        fn drop(&mut self) {
            // ignored: best-effort cleanup of a test temp dir; a leftover
            // directory is harmless and must not mask the test's own result.
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// Where `install.sh` (defaults, `PREFIX=/usr/local`) puts a manifest
    /// entry; mirrors the `case` of `uninstall_package()`.
    fn installed_path(entry: &ManifestEntry, paths: &RenderPaths) -> TestResult<String> {
        let dir = match entry.prefix.as_str() {
            "libexec" => paths.libexecdir.as_str(),
            "bpf" => paths.bpfdir.as_str(),
            "systemd" => UNIT_DIR,
            "sysusers" => SYSUSERS_DIR,
            "tmpfiles" => TMPFILES_DIR,
            "config" if entry.name == "config.toml.example" => paths.sysconfdir.as_str(),
            _ => {
                return Err(TestError::Unexpected(format!(
                    "no install location for manifest entry {}/{}",
                    entry.prefix, entry.name
                )));
            }
        };
        Ok(format!("{dir}/{}", entry.name))
    }

    /// The `deploy/` source of a manifest entry that `install.sh` installs
    /// from the canonical deployment tree, or `None` for build artifacts.
    fn deploy_source(entry: &ManifestEntry) -> Option<String> {
        let dir = match entry.prefix.as_str() {
            "systemd" => "deploy/systemd",
            "sysusers" => "deploy/sysusers.d",
            "tmpfiles" => "deploy/tmpfiles.d",
            _ => return None,
        };
        Some(format!("{dir}/{}", entry.name))
    }

    #[test]
    fn test_staged_install_resolves_every_unit_and_uninstall_removes_every_file() -> TestResult {
        let manifest = Manifest::load()?;
        let paths = RenderPaths::default();
        let stage = Stage::create(&manifest)?;

        stage.run("install")?;
        let expected = manifest
            .entries
            .iter()
            .map(|entry| installed_path(entry, &paths))
            .collect::<TestResult<BTreeSet<String>>>()?;
        assert_eq!(
            stage.files()?,
            expected,
            "install.sh installed a different file set than the manifest lists"
        );

        // Installed deployment files are exactly what `harw install
        // --print-systemd` prints: install.sh's sed renders like `render`.
        for entry in &manifest.entries {
            let Some(source) = deploy_source(entry) else {
                continue;
            };
            let embedded = asset(&source).ok_or_else(|| {
                TestError::Unexpected(format!("{source} is shipped but not embedded"))
            })?;
            let installed = fs::read_to_string(stage.path(&installed_path(entry, &paths)?))
                .map_err(ctx("read an installed deployment file"))?;
            assert_eq!(
                installed,
                render(embedded.contents, &paths),
                "{source}: the installed copy differs from the rendered embedded copy"
            );
            let leftover = unresolved_placeholders(&installed);
            assert!(
                leftover.is_empty(),
                "{source}: unresolved placeholders after install: {leftover:?}"
            );
        }

        // Every program an installed unit runs is an installed executable.
        let mut started = BTreeSet::new();
        for unit_name in manifest.names("systemd") {
            let text = fs::read_to_string(stage.path(&format!("{UNIT_DIR}/{unit_name}")))
                .map_err(ctx("read an installed unit"))?;
            let unit = parse_unit(&text);
            for key in EXEC_KEYS.iter().copied() {
                for value in unit.values("Service", key) {
                    let Some(binary) = packaged_program(unit_name, key, value, &paths)? else {
                        continue;
                    };
                    let program = format!("{}/{binary}", paths.libexecdir);
                    let metadata = fs::metadata(stage.path(&program)).map_err(|error| {
                        TestError::Unexpected(format!(
                            "{unit_name}: {key}= runs {program}, missing from the stage: {error}"
                        ))
                    })?;
                    assert!(
                        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
                        "{unit_name}: {program} is not an executable file in the stage"
                    );
                    started.insert(binary.to_owned());
                }
            }
        }
        for class in UNIT_CLASSES {
            assert!(
                started.contains(class.binary),
                "no installed unit starts {}",
                class.binary
            );
        }
        assert!(started.contains(NETLINK_PROBE));

        stage.run("uninstall")?;
        let left = stage.files()?;
        assert!(
            left.is_empty(),
            "install.sh uninstall left installed files behind: {left:?}"
        );
        // The stage is removed when it drops here.
        Ok(())
    }

    #[test]
    fn test_defaulted_variables_reads_only_upper_case_defaults() -> TestResult {
        let found = defaulted_variables(
            "a=${A:-x}\nb=${B_C:-${D2:-/v}/w}\nc=${1:-i}\nd=${e:-f}\ng=${h:+ ${I:-j}}\n",
        );
        assert_eq!(
            found.into_iter().collect::<Vec<&str>>(),
            ["A", "B_C", "D2", "I"]
        );
        Ok(())
    }

    /// The host branch of `uninstall_package()` (owner review 4505223360):
    /// with an empty `DESTDIR` it stops and disables every shipped unit,
    /// `harw-probe-netlink.service` included, one call per unit in manifest
    /// order and before the unit file is removed, then reloads systemd and
    /// removes every installed file.
    #[test]
    fn test_host_uninstall_disables_every_unit_before_removing_it() -> TestResult {
        // The run below takes install.sh's host branch, so every variable
        // that could point it outside the stage must be one it redirects.
        let script = read_repo_file("harw-dod/scripts/install.sh")?;
        let defaulted = defaulted_variables(&script);
        assert!(defaulted.contains("SYSTEMD_UNITDIR"), "{defaulted:?}");
        for name in &defaulted {
            assert!(
                INSTALLER_ENV.contains(name),
                "install.sh reads {name}, which the staged runs do not control"
            );
        }

        let manifest = Manifest::load()?;
        let paths = RenderPaths::default();
        let stage = Stage::create(&manifest)?;
        let bin = stage.root.join("host-bin");
        let log = stage.root.join("systemctl.log");
        fs::create_dir_all(&bin).map_err(ctx("create the fake host bin dir"))?;
        for (name, text) in [("id", FAKE_ID), ("systemctl", FAKE_SYSTEMCTL)] {
            let path = bin.join(name);
            fs::write(&path, text).map_err(ctx("write a fake host tool"))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))
                .map_err(ctx("make a fake host tool executable"))?;
        }
        for tool in HOST_UNINSTALL_TOOLS.iter().copied() {
            std::os::unix::fs::symlink(find_tool(tool)?, bin.join(tool))
                .map_err(ctx("link a host tool into the fake bin dir"))?;
        }

        stage.run("install")?;
        stage.run_host_uninstall(&bin, &log, &paths)?;

        let logged = fs::read_to_string(&log).map_err(ctx("read the fake systemctl log"))?;
        let mut expected: Vec<String> = manifest
            .names("systemd")
            .iter()
            .map(|unit| format!("disable --now {unit} present"))
            .collect();
        expected.push("daemon-reload absent".to_owned());
        assert_eq!(
            logged.lines().collect::<Vec<&str>>(),
            expected,
            "host uninstall must disable each shipped unit while its file exists, then reload"
        );
        assert!(
            expected.contains(&format!("disable --now {NETLINK_UNIT} present")),
            "the manifest ships no {NETLINK_UNIT}"
        );
        let left = stage.files()?;
        assert!(
            left.is_empty(),
            "host-path install.sh uninstall left installed files behind: {left:?}"
        );
        Ok(())
    }
}

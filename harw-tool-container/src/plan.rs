//! The run plan: a validated request plus trusted config becomes one
//! inspectable `podman run` argument vector. Nothing is executed here.
//!
//! # What the model controls and what it does not
//! The request carries an image (already resolved from an alias), a profile
//! name, a command, an optional relative working directory, allowlisted
//! environment entries and an optional shorter timeout. Everything else is
//! fixed by the profile or by trusted config: mounts, network, capabilities,
//! user namespace, memory, process count, pull policy, labels.
//!
//! # The argument vector
//! ```text
//! [--connection=NAME] run --rm --pull=never --name=harw-<run> [--cidfile=P]
//!   --label=harw.owner=<o> --label=harw.run=<r> --label=harw.profile=<p>
//!   --network=none --read-only --cap-drop=all --security-opt=no-new-privileges
//!   --pids-limit=<n> --memory=<n>m --memory-swap=<n>m --userns=keep-id --timeout=<s>
//!   --mount=... [--mount=...]  --workdir=/workspace[/<rel>]  [--env=K=V]
//!   -- <image@sha256:...> <command...>
//! ```
//! `--` ends option parsing and the image is the first positional argument,
//! so neither the image nor any command word can be read as an engine flag.
//! That `podman` and `docker` accept `--` before the image must still be
//! proven by an integration test (see `docs/research/container-and-cli-tools.md`,
//! section 9); this crate cannot run an engine.

use std::process::{Command, Stdio};

use crate::digest::argv_sha256_hex;
use crate::env::{DEFAULT_ENV_ALLOW, check_key, check_value};
use crate::error::ContainerPolicyError;
use crate::image::ImageRef;
use crate::mount::Mount;
use crate::profile::Profile;
use crate::validate::{check_abs_path, check_label_value, check_name, check_rel_path};

/// Fixed destination of the workspace inside the container.
pub const WORKSPACE_DST: &str = "/workspace";
/// Fixed destination of the cache volume inside the container.
pub const CACHE_DST: &str = "/cache";

/// Most command words accepted.
const MAX_COMMAND_ARGS: usize = 256;
/// Longest single command word in bytes.
const MAX_ARG_BYTES: usize = 8192;
/// Longest whole command in bytes.
const MAX_COMMAND_BYTES: usize = 64 * 1024;
/// Most environment entries accepted.
const MAX_ENV_ENTRIES: usize = 32;

/// Trusted per-run configuration. Never built from model input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunConfig {
    executable: String,
    workspace: String,
    run_id: String,
    owner: String,
    connection: Option<String>,
    cache_volume: Option<String>,
    extra_mounts: Vec<Mount>,
    env_allow: Vec<String>,
    cidfile: Option<String>,
}

impl RunConfig {
    /// Starts a configuration.
    ///
    /// `executable` is an absolute path whose file name is `podman` (no
    /// `PATH` search). `workspace` is an absolute, canonical host path.
    /// `run_id` is `[a-z0-9][a-z0-9._-]{0,47}` and names the container
    /// (`harw-<run_id>`). `owner` is a label value that identifies the
    /// session or runner.
    ///
    /// # Errors
    /// The first rule that a value breaks.
    pub fn new(
        executable: &str,
        workspace: &str,
        run_id: &str,
        owner: &str,
    ) -> Result<Self, ContainerPolicyError> {
        check_abs_path(executable)?;
        if executable.rsplit('/').next() != Some("podman") {
            return Err(ContainerPolicyError::InvalidPath(
                "the engine executable must be named podman",
            ));
        }
        check_abs_path(workspace)?;
        check_name(run_id, 48, "run id")?;
        check_label_value(owner, "owner")?;
        Ok(Self {
            executable: executable.to_owned(),
            workspace: workspace.to_owned(),
            run_id: run_id.to_owned(),
            owner: owner.to_owned(),
            connection: None,
            cache_volume: None,
            extra_mounts: Vec::new(),
            env_allow: DEFAULT_ENV_ALLOW.iter().map(|k| (*k).to_owned()).collect(),
            cidfile: None,
        })
    }

    /// Uses a named remote connection (`podman --connection NAME`).
    ///
    /// # Errors
    /// An invalid connection name.
    pub fn with_connection(mut self, name: &str) -> Result<Self, ContainerPolicyError> {
        check_name(name, 64, "connection")?;
        self.connection = Some(name.to_owned());
        Ok(self)
    }

    /// Names the cache volume mounted at `/cache` in the build profile.
    ///
    /// # Errors
    /// An invalid volume name.
    pub fn with_cache_volume(mut self, name: &str) -> Result<Self, ContainerPolicyError> {
        check_name(name, 64, "cache volume")?;
        self.cache_volume = Some(name.to_owned());
        Ok(self)
    }

    /// Adds a trusted extra mount. Its destination must be unused.
    ///
    /// # Errors
    /// A destination that is `/workspace`, `/cache` or already used.
    pub fn with_mount(mut self, mount: Mount) -> Result<Self, ContainerPolicyError> {
        let used = mount.dst() == WORKSPACE_DST
            || mount.dst() == CACHE_DST
            || self.extra_mounts.iter().any(|m| m.dst() == mount.dst());
        if used {
            return Err(ContainerPolicyError::InvalidMount(
                "destination already used",
            ));
        }
        self.extra_mounts.push(mount);
        Ok(self)
    }

    /// Replaces the container environment allowlist.
    ///
    /// # Errors
    /// An invalid environment key.
    pub fn with_env_allow<'a>(
        mut self,
        keys: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, ContainerPolicyError> {
        let mut allow = Vec::new();
        for key in keys {
            check_key(key)?;
            allow.push(key.to_owned());
        }
        self.env_allow = allow;
        Ok(self)
    }

    /// Writes the container id to this absolute path (`--cidfile`).
    ///
    /// # Errors
    /// An invalid path.
    pub fn with_cidfile(mut self, path: &str) -> Result<Self, ContainerPolicyError> {
        check_abs_path(path)?;
        self.cidfile = Some(path.to_owned());
        Ok(self)
    }
}

/// What the model asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    image: ImageRef,
    profile: Profile,
    command: Vec<String>,
    workdir: Option<String>,
    env: Vec<(String, String)>,
    timeout_s: Option<u32>,
}

impl RunRequest {
    /// A request to run `command` in `image` under `profile`.
    ///
    /// # Errors
    /// An empty, oversized or NUL-containing command.
    pub fn new(
        image: ImageRef,
        profile: Profile,
        command: Vec<String>,
    ) -> Result<Self, ContainerPolicyError> {
        let err = ContainerPolicyError::InvalidCommand;
        if command.is_empty() {
            return Err(err("empty"));
        }
        if command.len() > MAX_COMMAND_ARGS {
            return Err(err("too many words"));
        }
        if command.first().is_some_and(String::is_empty) {
            return Err(err("empty program"));
        }
        let mut total = 0usize;
        for word in &command {
            if word.len() > MAX_ARG_BYTES {
                return Err(err("a word is too long"));
            }
            if word.contains('\0') {
                return Err(err("NUL in a word"));
            }
            total += word.len();
        }
        if total > MAX_COMMAND_BYTES {
            return Err(err("too long"));
        }
        Ok(Self {
            image,
            profile,
            command,
            workdir: None,
            env: Vec::new(),
            timeout_s: None,
        })
    }

    /// Sets a working directory relative to `/workspace`.
    ///
    /// # Errors
    /// An absolute path, `..`, or a forbidden character.
    pub fn with_workdir(mut self, relative: &str) -> Result<Self, ContainerPolicyError> {
        check_rel_path(relative)?;
        self.workdir = Some(relative.to_owned());
        Ok(self)
    }

    /// Adds an environment entry. The key is checked against the config
    /// allowlist when the plan is built.
    ///
    /// # Errors
    /// A malformed key or value, or too many entries.
    pub fn with_env(mut self, key: &str, value: &str) -> Result<Self, ContainerPolicyError> {
        check_key(key)?;
        check_value(value)?;
        if self.env.len() >= MAX_ENV_ENTRIES {
            return Err(ContainerPolicyError::InvalidEnv("too many entries"));
        }
        self.env.push((key.to_owned(), value.to_owned()));
        Ok(self)
    }

    /// Asks for a timeout shorter than the profile ceiling. Longer values are
    /// clamped to the ceiling, zero to one second.
    #[must_use]
    pub fn with_timeout_s(mut self, seconds: u32) -> Self {
        self.timeout_s = Some(seconds);
        self
    }
}

/// What the engine must report after creation; input of
/// [`crate::readback::verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    /// The workspace mount is read-only.
    pub workspace_read_only: bool,
    /// Memory ceiling in bytes.
    pub memory_bytes: i64,
    /// Process ceiling.
    pub pids_limit: i64,
}

/// A validated, inspectable container run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerPlan {
    executable: String,
    args: Vec<String>,
    name: String,
    profile: Profile,
    image: ImageRef,
    timeout_s: u32,
    expected: Expected,
    approval: String,
}

impl ContainerPlan {
    /// Builds the plan.
    ///
    /// # Errors
    /// Any rule broken by the config, the request, or their combination
    /// (an extra writable mount in the hermetic profile, an environment key
    /// off the allowlist, a workspace in a denied tree).
    pub fn build(config: &RunConfig, request: &RunRequest) -> Result<Self, ContainerPolicyError> {
        let profile = request.profile;
        let limits = profile.limits();
        let timeout_s = request
            .timeout_s
            .map_or(limits.timeout_s, |t| t.clamp(1, limits.timeout_s));

        let mut mounts = vec![Mount::bind(
            &config.workspace,
            WORKSPACE_DST,
            profile.workspace_read_only(),
        )?];
        if profile.allows_cache() {
            if let Some(volume) = &config.cache_volume {
                mounts.push(Mount::volume(volume, CACHE_DST, false)?);
            }
        }
        for extra in &config.extra_mounts {
            if profile == Profile::Hermetic && !extra.read_only() {
                return Err(ContainerPolicyError::InvalidMount(
                    "the hermetic profile allows read-only mounts only",
                ));
            }
            mounts.push(extra.clone());
        }

        let mut seen: Vec<&str> = Vec::new();
        for (key, _) in &request.env {
            if !config.env_allow.iter().any(|allowed| allowed == key) {
                return Err(ContainerPolicyError::EnvKeyNotAllowed);
            }
            if seen.contains(&key.as_str()) {
                return Err(ContainerPolicyError::InvalidEnv("duplicate key"));
            }
            seen.push(key);
        }

        let name = format!("harw-{}", config.run_id);
        let workdir = request.workdir.as_ref().map_or_else(
            || WORKSPACE_DST.to_owned(),
            |rel| format!("{WORKSPACE_DST}/{rel}"),
        );

        let mut args: Vec<String> = Vec::new();
        if let Some(connection) = &config.connection {
            args.push(format!("--connection={connection}"));
        }
        args.extend(["run", "--rm", "--pull=never"].map(str::to_owned));
        args.push(format!("--name={name}"));
        if let Some(cidfile) = &config.cidfile {
            args.push(format!("--cidfile={cidfile}"));
        }
        args.push(format!("--label=harw.owner={}", config.owner));
        args.push(format!("--label=harw.run={}", config.run_id));
        args.push(format!("--label=harw.profile={profile}"));
        args.extend(
            [
                "--network=none",
                "--read-only",
                "--cap-drop=all",
                "--security-opt=no-new-privileges",
            ]
            .map(str::to_owned),
        );
        args.push(format!("--pids-limit={}", limits.pids));
        args.push(format!("--memory={}m", limits.memory_mib));
        // Podman defaults swap to twice the memory limit; equal values mean no swap.
        args.push(format!("--memory-swap={}m", limits.memory_mib));
        args.push("--userns=keep-id".to_owned());
        args.push(format!("--timeout={timeout_s}"));
        for mount in &mounts {
            args.push(mount.to_arg());
        }
        args.push(format!("--workdir={workdir}"));
        for (key, value) in &request.env {
            args.push(format!("--env={key}={value}"));
        }
        args.push("--".to_owned());
        args.push(request.image.to_arg());
        args.extend(request.command.iter().cloned());

        let expected = Expected {
            workspace_read_only: profile.workspace_read_only(),
            memory_bytes: i64::from(limits.memory_mib) * 1024 * 1024,
            pids_limit: i64::from(limits.pids),
        };
        let approval = approval_text(config, request, &mounts, timeout_s, &workdir);
        Ok(Self {
            executable: config.executable.clone(),
            args,
            name,
            profile,
            image: request.image.clone(),
            timeout_s,
            expected,
            approval,
        })
    }

    /// The engine executable (absolute path).
    #[must_use]
    pub fn executable(&self) -> &str {
        &self.executable
    }

    /// The complete argument vector, to be passed one element per argument.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// The container name (`harw-<run id>`).
    #[must_use]
    pub fn container_name(&self) -> &str {
        &self.name
    }

    /// The profile.
    #[must_use]
    pub fn profile(&self) -> Profile {
        self.profile
    }

    /// The image.
    #[must_use]
    pub fn image(&self) -> &ImageRef {
        &self.image
    }

    /// The effective timeout after clamping.
    #[must_use]
    pub fn timeout_s(&self) -> u32 {
        self.timeout_s
    }

    /// What the engine must report after creation.
    #[must_use]
    pub fn expected(&self) -> &Expected {
        &self.expected
    }

    /// Canonical approval text. Two plans with the same text are the same
    /// grant; any change to image, profile, mounts, environment keys,
    /// timeout, working directory or command changes the text.
    #[must_use]
    pub fn approval_text(&self) -> &str {
        &self.approval
    }

    /// The argument vector with environment values hidden, for logs.
    #[must_use]
    pub fn redacted_args(&self) -> Vec<String> {
        self.args
            .iter()
            .map(|arg| match arg.strip_prefix("--env=") {
                Some(rest) => {
                    let key = rest.split_once('=').map_or(rest, |(k, _)| k);
                    format!("--env={key}=***")
                }
                None => arg.clone(),
            })
            .collect()
    }

    /// Builds the process description. The environment is cleared and only
    /// `engine_env` is set (see [`crate::env::engine_environment`]); stdin is
    /// closed. Constructing the command does not start anything.
    #[must_use]
    pub fn to_command(&self, engine_env: &[(String, String)]) -> Command {
        let mut command = Command::new(&self.executable);
        command.args(&self.args);
        command.env_clear();
        command.envs(engine_env.iter().map(|(k, v)| (k, v)));
        command.stdin(Stdio::null());
        command
    }
}

/// The approval text names the program and binds the full command by digest.
/// The arguments themselves are never shown or persisted: they may carry
/// credentials, authorization headers or signed URLs.
fn approval_text(
    config: &RunConfig,
    request: &RunRequest,
    mounts: &[Mount],
    timeout_s: u32,
    workdir: &str,
) -> String {
    let mount_list: Vec<String> = mounts.iter().map(Mount::to_arg).collect();
    let env_keys: Vec<&str> = request.env.iter().map(|(k, _)| k.as_str()).collect();
    format!(
        "engine=podman\nconnection={}\nimage={}\nprofile={}\nnetwork=none\nmounts={:?}\n\
         env_keys={:?}\ntimeout_s={timeout_s}\nworkdir={workdir}\nprogram={}\nargc={}\n\
         command_sha256={}",
        config.connection.as_deref().unwrap_or("local"),
        request.image.to_arg(),
        request.profile,
        mount_list,
        env_keys,
        request.command.first().map_or("", String::as_str),
        request.command.len(),
        argv_sha256_hex(&request.command),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readback::{InspectFacts, verify};
    use crate::test_support::{TestResult, ensure};

    const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn image() -> Result<ImageRef, ContainerPolicyError> {
        ImageRef::parse(&format!("docker.io/library/rust@sha256:{HEX}"))
    }

    fn config() -> Result<RunConfig, ContainerPolicyError> {
        RunConfig::new("/usr/bin/podman", "/srv/ws", "r1", "ses1")
    }

    fn cmd(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    fn request(profile: Profile) -> Result<RunRequest, ContainerPolicyError> {
        RunRequest::new(image()?, profile, cmd(&["cargo", "check"]))
    }

    fn strings(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn hermetic_argv_is_exact() -> TestResult {
        let plan = ContainerPlan::build(&config()?, &request(Profile::Hermetic)?)?;
        let want = strings(&[
            "run",
            "--rm",
            "--pull=never",
            "--name=harw-r1",
            "--label=harw.owner=ses1",
            "--label=harw.run=r1",
            "--label=harw.profile=hermetic",
            "--network=none",
            "--read-only",
            "--cap-drop=all",
            "--security-opt=no-new-privileges",
            "--pids-limit=256",
            "--memory=1024m",
            "--memory-swap=1024m",
            "--userns=keep-id",
            "--timeout=300",
            "--mount=type=bind,src=/srv/ws,dst=/workspace,ro",
            "--workdir=/workspace",
            "--",
            &format!("docker.io/library/rust@sha256:{HEX}"),
            "cargo",
            "check",
        ]);
        ensure(
            plan.args() == want.as_slice(),
            &format!("{:?}", plan.args()),
        )?;
        ensure(plan.executable() == "/usr/bin/podman", "executable")?;
        ensure(plan.container_name() == "harw-r1", "name")
    }

    #[test]
    fn build_profile_is_writable_with_a_cache() -> TestResult {
        let cfg = config()?.with_cache_volume("harw-cargo")?;
        let plan = ContainerPlan::build(&cfg, &request(Profile::Build)?)?;
        let a = plan.args();
        ensure(
            a.contains(&"--mount=type=bind,src=/srv/ws,dst=/workspace".to_owned()),
            "workspace rw",
        )?;
        ensure(
            a.contains(&"--mount=type=volume,src=harw-cargo,dst=/cache".to_owned()),
            "cache",
        )?;
        ensure(a.contains(&"--memory=4096m".to_owned()), "memory")?;
        ensure(
            a.contains(&"--memory-swap=4096m".to_owned()),
            "no swap beyond memory",
        )?;
        ensure(a.contains(&"--pids-limit=512".to_owned()), "pids")?;
        ensure(a.contains(&"--timeout=1800".to_owned()), "timeout")?;
        ensure(a.contains(&"--network=none".to_owned()), "still no network")
    }

    #[test]
    fn hermetic_never_mounts_the_cache() -> TestResult {
        let cfg = config()?.with_cache_volume("harw-cargo")?;
        let plan = ContainerPlan::build(&cfg, &request(Profile::Hermetic)?)?;
        ensure(
            !plan.args().iter().any(|a| a.contains("type=volume")),
            "no volume",
        )
    }

    #[test]
    fn hardening_flags_are_always_present() -> TestResult {
        for profile in Profile::ALL {
            let plan = ContainerPlan::build(&config()?, &request(profile)?)?;
            for flag in [
                "--rm",
                "--pull=never",
                "--network=none",
                "--read-only",
                "--cap-drop=all",
                "--security-opt=no-new-privileges",
                "--userns=keep-id",
            ] {
                ensure(plan.args().iter().any(|a| a == flag), flag)?;
            }
        }
        Ok(())
    }

    #[test]
    fn the_image_follows_the_separator_and_the_command_follows_the_image() -> TestResult {
        let req = RunRequest::new(
            image()?,
            Profile::Hermetic,
            cmd(&["--privileged", "-v", "/:/host"]),
        )?;
        let plan = ContainerPlan::build(&config()?, &req)?;
        let a = plan.args();
        let sep = a
            .iter()
            .position(|x| x == "--")
            .ok_or_else(|| crate::test_support::TestError::Unexpected("no separator".to_owned()))?;
        ensure(
            a[sep + 1] == format!("docker.io/library/rust@sha256:{HEX}"),
            "image",
        )?;
        ensure(
            a[sep + 2..] == ["--privileged", "-v", "/:/host"],
            "command verbatim",
        )?;
        ensure(
            !a[..sep].iter().any(|x| x == "--privileged" || x == "-v"),
            "no injected option before the separator",
        )
    }

    #[test]
    fn timeout_is_clamped_to_the_profile_ceiling() -> TestResult {
        let long = request(Profile::Hermetic)?.with_timeout_s(99_999);
        ensure(
            ContainerPlan::build(&config()?, &long)?.timeout_s() == 300,
            "ceiling",
        )?;
        let short = request(Profile::Hermetic)?.with_timeout_s(30);
        ensure(
            ContainerPlan::build(&config()?, &short)?.timeout_s() == 30,
            "shorter",
        )?;
        let zero = request(Profile::Hermetic)?.with_timeout_s(0);
        ensure(
            ContainerPlan::build(&config()?, &zero)?.timeout_s() == 1,
            "floor",
        )
    }

    #[test]
    fn a_connection_precedes_run() -> TestResult {
        let cfg = config()?.with_connection("pi")?;
        let plan = ContainerPlan::build(&cfg, &request(Profile::Hermetic)?)?;
        ensure(
            plan.args()[0] == "--connection=pi" && plan.args()[1] == "run",
            "order",
        )?;
        ensure(config()?.with_connection("a b").is_err(), "bad name")
    }

    #[test]
    fn a_cidfile_is_passed_when_configured() -> TestResult {
        let cfg = config()?.with_cidfile("/srv/job/cid")?;
        let plan = ContainerPlan::build(&cfg, &request(Profile::Hermetic)?)?;
        ensure(
            plan.args().contains(&"--cidfile=/srv/job/cid".to_owned()),
            "cidfile",
        )?;
        ensure(config()?.with_cidfile("relative").is_err(), "relative")
    }

    #[test]
    fn environment_is_allowlisted_deduplicated_and_redacted() -> TestResult {
        let ok = request(Profile::Hermetic)?.with_env("RUST_LOG", "secret-looking")?;
        let plan = ContainerPlan::build(&config()?, &ok)?;
        ensure(
            plan.args()
                .contains(&"--env=RUST_LOG=secret-looking".to_owned()),
            "passed",
        )?;
        ensure(
            plan.redacted_args()
                .contains(&"--env=RUST_LOG=***".to_owned()),
            "redacted",
        )?;
        ensure(
            !plan
                .redacted_args()
                .iter()
                .any(|a| a.contains("secret-looking")),
            "value hidden",
        )?;
        let off = request(Profile::Hermetic)?.with_env("AWS_SECRET_ACCESS_KEY", "x")?;
        ensure(
            ContainerPlan::build(&config()?, &off) == Err(ContainerPolicyError::EnvKeyNotAllowed),
            "not on the allowlist",
        )?;
        let dup = request(Profile::Hermetic)?
            .with_env("CI", "1")?
            .with_env("CI", "2")?;
        ensure(ContainerPlan::build(&config()?, &dup).is_err(), "duplicate")?;
        let widened = config()?.with_env_allow(["MY_FLAG"])?;
        let req = request(Profile::Hermetic)?.with_env("MY_FLAG", "1")?;
        ensure(
            ContainerPlan::build(&widened, &req).is_ok(),
            "configured key",
        )?;
        let default_key = request(Profile::Hermetic)?.with_env("CI", "1")?;
        ensure(
            ContainerPlan::build(&widened, &default_key).is_err(),
            "defaults replaced",
        )
    }

    #[test]
    fn workdir_stays_below_the_workspace() -> TestResult {
        let ok = request(Profile::Hermetic)?.with_workdir("crates/x")?;
        let plan = ContainerPlan::build(&config()?, &ok)?;
        ensure(
            plan.args()
                .contains(&"--workdir=/workspace/crates/x".to_owned()),
            "relative",
        )?;
        for bad in ["/etc", "../x", "a/../b", "a:b", ""] {
            ensure(request(Profile::Hermetic)?.with_workdir(bad).is_err(), bad)?;
        }
        Ok(())
    }

    #[test]
    fn extra_mounts_follow_the_profile() -> TestResult {
        let rw = Mount::bind("/srv/data", "/data", false)?;
        let ro = Mount::bind("/srv/ref", "/mnt/ref", true)?;
        let cfg = config()?.with_mount(rw)?.with_mount(ro)?;
        ensure(
            ContainerPlan::build(&cfg, &request(Profile::Hermetic)?).is_err(),
            "rw extra mount in hermetic",
        )?;
        let plan = ContainerPlan::build(&cfg, &request(Profile::Build)?)?;
        ensure(
            plan.args()
                .contains(&"--mount=type=bind,src=/srv/data,dst=/data".to_owned()),
            "rw in build",
        )?;
        let clash = Mount::bind("/srv/other", "/workspace", true)?;
        ensure(
            config()?.with_mount(clash).is_err(),
            "workspace destination",
        )?;
        let dup1 = Mount::bind("/srv/a", "/data", true)?;
        let dup2 = Mount::bind("/srv/b", "/data", true)?;
        ensure(
            config()?.with_mount(dup1)?.with_mount(dup2).is_err(),
            "duplicate destination",
        )
    }

    #[test]
    fn config_rejects_bad_values() -> TestResult {
        ensure(
            RunConfig::new("/usr/bin/docker", "/srv/ws", "r1", "o").is_err(),
            "not podman",
        )?;
        ensure(
            RunConfig::new("podman", "/srv/ws", "r1", "o").is_err(),
            "relative executable",
        )?;
        ensure(
            RunConfig::new("/usr/bin/podman", "srv/ws", "r1", "o").is_err(),
            "relative ws",
        )?;
        ensure(
            RunConfig::new("/usr/bin/podman", "/srv/ws", "R 1", "o").is_err(),
            "run id",
        )?;
        ensure(
            RunConfig::new("/usr/bin/podman", "/srv/ws", "r1", "o p").is_err(),
            "owner",
        )
    }

    #[test]
    fn a_workspace_in_a_denied_tree_fails_the_build() -> TestResult {
        for ws in ["/", "/etc/app", "/run/user/1000", "/home/u/.ssh/keys"] {
            let cfg = RunConfig::new("/usr/bin/podman", ws, "r1", "o")?;
            ensure(
                ContainerPlan::build(&cfg, &request(Profile::Hermetic)?).is_err(),
                ws,
            )?;
        }
        Ok(())
    }

    #[test]
    fn commands_are_bounded() -> TestResult {
        let img = image()?;
        ensure(
            RunRequest::new(img.clone(), Profile::Hermetic, vec![]).is_err(),
            "empty",
        )?;
        ensure(
            RunRequest::new(img.clone(), Profile::Hermetic, cmd(&[""])).is_err(),
            "empty program",
        )?;
        ensure(
            RunRequest::new(img.clone(), Profile::Hermetic, cmd(&["a\0b"])).is_err(),
            "nul",
        )?;
        ensure(
            RunRequest::new(img.clone(), Profile::Hermetic, vec!["x".to_owned(); 257]).is_err(),
            "too many",
        )?;
        ensure(
            RunRequest::new(img, Profile::Hermetic, vec!["x".repeat(8193)]).is_err(),
            "too long",
        )
    }

    #[test]
    fn approval_text_changes_with_every_grant_relevant_input() -> TestResult {
        let base = ContainerPlan::build(&config()?, &request(Profile::Hermetic)?)?;
        let same = ContainerPlan::build(&config()?, &request(Profile::Hermetic)?)?;
        ensure(base.approval_text() == same.approval_text(), "stable")?;
        let other_cmd = RunRequest::new(image()?, Profile::Hermetic, cmd(&["cargo", "test"]))?;
        let other_img = RunRequest::new(
            ImageRef::parse(&format!("docker.io/library/rust@sha256:{}", "f".repeat(64)))?,
            Profile::Hermetic,
            cmd(&["cargo", "check"]),
        )?;
        let with_env = request(Profile::Hermetic)?.with_env("CI", "1")?;
        let with_dir = request(Profile::Hermetic)?.with_workdir("x")?;
        let shorter = request(Profile::Hermetic)?.with_timeout_s(10);
        for (label, req) in [
            ("command", other_cmd),
            ("image", other_img),
            ("env", with_env),
            ("workdir", with_dir),
            ("timeout", shorter),
            ("profile", request(Profile::Build)?),
        ] {
            let plan = ContainerPlan::build(&config()?, &req)?;
            ensure(plan.approval_text() != base.approval_text(), label)?;
        }
        let remote = ContainerPlan::build(
            &config()?.with_connection("pi")?,
            &request(Profile::Hermetic)?,
        )?;
        ensure(remote.approval_text() != base.approval_text(), "connection")
    }

    #[test]
    fn approval_text_does_not_contain_env_values() -> TestResult {
        let req = request(Profile::Hermetic)?.with_env("CI", "hunter2")?;
        let plan = ContainerPlan::build(&config()?, &req)?;
        ensure(
            !plan.approval_text().contains("hunter2"),
            "no values in approval",
        )
    }

    #[test]
    fn approval_text_hides_command_arguments_but_binds_them() -> TestResult {
        let secret = "Authorization: Bearer s3cr3t-token";
        let make = |last: &str| -> Result<ContainerPlan, ContainerPolicyError> {
            let req = RunRequest::new(
                image()?,
                Profile::Hermetic,
                cmd(&["curl", "-H", secret, last]),
            )?;
            ContainerPlan::build(&config()?, &req)
        };
        let a = make("https://example.test/a")?;
        let b = make("https://example.test/b")?;
        ensure(!a.approval_text().contains("s3cr3t"), "secret not shown")?;
        ensure(!a.approval_text().contains("example.test"), "no arguments")?;
        ensure(a.approval_text().contains("program=curl"), "program named")?;
        ensure(a.approval_text().contains("argc=4"), "argument count")?;
        ensure(
            a.approval_text() != b.approval_text(),
            "a changed argument invalidates the approval",
        )
    }

    #[test]
    fn expected_matches_the_profile_and_round_trips_through_readback() -> TestResult {
        let plan = ContainerPlan::build(&config()?, &request(Profile::Hermetic)?)?;
        ensure(plan.expected().memory_bytes == 1024 * 1024 * 1024, "memory")?;
        ensure(plan.expected().pids_limit == 256, "pids")?;
        ensure(plan.expected().workspace_read_only, "ro")?;
        let facts = InspectFacts {
            privileged: Some(false),
            cap_add: Some(vec![]),
            cap_drop: Some(vec!["all".to_owned()]),
            network_mode: Some("none".to_owned()),
            read_only_rootfs: Some(true),
            security_opt: Some(vec!["no-new-privileges".to_owned()]),
            memory_bytes: Some(plan.expected().memory_bytes),
            memory_swap_bytes: Some(plan.expected().memory_bytes),
            effective_caps: Some(vec![]),
            pids_limit: Some(plan.expected().pids_limit),
            workspace_read_only: Some(true),
        };
        ensure(verify(plan.expected(), &facts).all_enforced(), "enforced")
    }

    #[test]
    fn the_command_has_a_cleared_environment() -> TestResult {
        let plan = ContainerPlan::build(&config()?, &request(Profile::Hermetic)?)?;
        let env = vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())];
        let command = plan.to_command(&env);
        ensure(command.get_program() == "/usr/bin/podman", "program")?;
        ensure(command.get_args().count() == plan.args().len(), "args")?;
        ensure(command.get_envs().count() == 1, "only the given env")
    }
}

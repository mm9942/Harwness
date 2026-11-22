//! Device tools: Termux shell and SAF file access, implemented by Kotlin.
//!
//! This module owns the shared limits, the request validation and the path
//! rules. The platform side mirrors the path rules in `normalizeSafPath`.

const MAX_ARGS: u32 = 64;
const MAX_ARG_BYTES: u32 = 4096;
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 120_000;
const MAX_OUTPUT_BYTES: u32 = 65_536;
const MAX_READ_BYTES: u64 = 8_388_608;
const MAX_WRITE_BYTES: u64 = 8_388_608;
const MAX_PATH_BYTES: u32 = 1024;
const MAX_SEGMENT_BYTES: usize = 255;
const TERMUX_HOME_PREFIX: &str = "/data/data/com.termux/files/";

/// Failure of a device tool.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum DeviceError {
    #[error("this device tool is switched off")]
    Disabled,
    #[error("device tool unavailable: {detail}")]
    Unavailable { detail: String },
    #[error("the request was declined")]
    Declined,
    #[error("the command timed out")]
    TimedOut,
    #[error("invalid request: {detail}")]
    Invalid { detail: String },
    #[error("invalid path: {detail}")]
    InvalidPath { detail: String },
    #[error("not found")]
    NotFound,
    #[error("the file already exists")]
    Exists,
    #[error("too large")]
    TooLarge,
    #[error("device tool failed: {detail}")]
    Failed { detail: String },
}

impl From<uniffi::UnexpectedUniFFICallbackError> for DeviceError {
    fn from(_: uniffi::UnexpectedUniFFICallbackError) -> Self {
        // The payload is deliberately ignored: it must never be formatted.
        Self::Failed {
            detail: "unexpected platform failure".to_owned(),
        }
    }
}

/// Hard limits shared by Rust and the platform side.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DeviceLimits {
    pub max_args: u32,
    pub max_arg_bytes: u32,
    pub default_timeout_ms: u64,
    pub max_timeout_ms: u64,
    pub max_output_bytes: u32,
    pub max_read_bytes: u64,
    pub max_write_bytes: u64,
    pub max_path_bytes: u32,
}

/// One shell command to run in Termux.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ShellRequest {
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    pub timeout_ms: u64,
    pub max_output_bytes: u32,
}

/// Result of a finished shell command.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ShellOutput {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    pub timed_out: bool,
}

/// A directory tree the user granted through the system picker.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SafRoot {
    pub id: String,
    pub label: String,
    pub writable: bool,
}

/// One entry of a granted directory tree.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SafEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size_bytes: u64,
    pub modified_ms: Option<i64>,
}

/// Shell access implemented by Kotlin on top of Termux.
#[uniffi::export(with_foreign)]
pub trait ShellExecutor: Send + Sync {
    fn available(&self) -> bool;
    fn run(&self, request: ShellRequest) -> Result<ShellOutput, DeviceError>;
}

/// File access implemented by Kotlin on top of the storage access framework.
#[uniffi::export(with_foreign)]
pub trait FileAccess: Send + Sync {
    fn roots(&self) -> Result<Vec<SafRoot>, DeviceError>;
    fn list(&self, root_id: String, path: String) -> Result<Vec<SafEntry>, DeviceError>;
    fn read(&self, root_id: String, path: String, max_bytes: u64) -> Result<Vec<u8>, DeviceError>;
    /// Create-only: an existing file yields `Exists`.
    fn write(&self, root_id: String, path: String, data: Vec<u8>) -> Result<(), DeviceError>;
}

/// The limits every device tool enforces.
#[uniffi::export]
pub fn device_limits() -> DeviceLimits {
    DeviceLimits {
        max_args: MAX_ARGS,
        max_arg_bytes: MAX_ARG_BYTES,
        default_timeout_ms: DEFAULT_TIMEOUT_MS,
        max_timeout_ms: MAX_TIMEOUT_MS,
        max_output_bytes: MAX_OUTPUT_BYTES,
        max_read_bytes: MAX_READ_BYTES,
        max_write_bytes: MAX_WRITE_BYTES,
        max_path_bytes: MAX_PATH_BYTES,
    }
}

fn invalid(detail: &str) -> DeviceError {
    DeviceError::Invalid {
        detail: detail.to_owned(),
    }
}

fn invalid_path(detail: &str) -> DeviceError {
    DeviceError::InvalidPath {
        detail: detail.to_owned(),
    }
}

/// Validate a shell request and return it with clamped timeout and output cap.
#[uniffi::export]
pub fn check_shell_request(request: ShellRequest) -> Result<ShellRequest, DeviceError> {
    if request.argv.is_empty() {
        return Err(invalid("empty command"));
    }
    if request.argv.len() > MAX_ARGS as usize {
        return Err(invalid("too many arguments"));
    }
    let program_empty = request.argv.first().is_none_or(String::is_empty);
    let bad_arg = request
        .argv
        .iter()
        .any(|arg| arg.len() > MAX_ARG_BYTES as usize || arg.contains('\0'));
    if program_empty || bad_arg {
        return Err(invalid("invalid argument"));
    }
    let cwd = match request.cwd {
        Some(cwd) => Some(check_cwd(cwd)?),
        None => None,
    };
    let timeout_ms = match request.timeout_ms {
        0 => DEFAULT_TIMEOUT_MS,
        value => value.min(MAX_TIMEOUT_MS),
    };
    let max_output_bytes = match request.max_output_bytes {
        0 => MAX_OUTPUT_BYTES,
        value => value.min(MAX_OUTPUT_BYTES),
    };
    Ok(ShellRequest {
        argv: request.argv,
        cwd,
        timeout_ms,
        max_output_bytes,
    })
}

fn check_cwd(cwd: String) -> Result<String, DeviceError> {
    if cwd.starts_with(TERMUX_HOME_PREFIX) {
        let escapes = cwd.contains('\0')
            || cwd.len() > MAX_PATH_BYTES as usize
            || cwd.split('/').any(|segment| segment == "..");
        if escapes {
            return Err(invalid_path("working directory outside Termux home"));
        }
        return Ok(cwd);
    }
    normalize_device_path(cwd)
}

/// Normalise a relative, `/`-separated path, rejecting anything that could escape.
#[uniffi::export]
pub fn normalize_device_path(path: String) -> Result<String, DeviceError> {
    if path.len() > MAX_PATH_BYTES as usize {
        return Err(invalid_path("path too long"));
    }
    if path.contains('\\') {
        return Err(invalid_path("backslash"));
    }
    if path.contains('\0') {
        return Err(invalid_path("nul byte"));
    }
    if path.starts_with('/') {
        return Err(invalid_path("absolute path"));
    }
    let trimmed = path.strip_suffix('/').unwrap_or(&path);
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    for segment in trimmed.split('/') {
        if segment.is_empty() {
            return Err(invalid_path("empty segment"));
        }
        if segment == "." || segment == ".." {
            return Err(invalid_path("dot segment"));
        }
        if segment.len() > MAX_SEGMENT_BYTES {
            return Err(invalid_path("segment too long"));
        }
    }
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn request(argv: &[&str]) -> ShellRequest {
        ShellRequest {
            argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
            cwd: None,
            timeout_ms: 0,
            max_output_bytes: 0,
        }
    }

    fn path_err(detail: &str) -> Result<String, DeviceError> {
        Err(DeviceError::InvalidPath {
            detail: detail.to_owned(),
        })
    }

    #[test]
    fn limits_match_the_contract() {
        let limits = device_limits();
        assert_eq!(limits.max_args, 64);
        assert_eq!(limits.max_arg_bytes, 4096);
        assert_eq!(limits.default_timeout_ms, 30_000);
        assert_eq!(limits.max_timeout_ms, 120_000);
        assert_eq!(limits.max_output_bytes, 65_536);
        assert_eq!(limits.max_read_bytes, 8_388_608);
        assert_eq!(limits.max_write_bytes, 8_388_608);
        assert_eq!(limits.max_path_bytes, 1024);
    }

    #[test]
    fn shell_request_clamps_timeout_and_output() -> TestResult {
        let checked = check_shell_request(request(&["ls"]))?;
        assert_eq!(checked.timeout_ms, 30_000);
        assert_eq!(checked.max_output_bytes, 65_536);
        let mut big = request(&["ls"]);
        big.timeout_ms = 999_999;
        big.max_output_bytes = 10_000_000;
        let checked = check_shell_request(big)?;
        assert_eq!(checked.timeout_ms, 120_000);
        assert_eq!(checked.max_output_bytes, 65_536);
        let mut small = request(&["ls"]);
        small.timeout_ms = 500;
        small.max_output_bytes = 100;
        let checked = check_shell_request(small)?;
        assert_eq!(checked.timeout_ms, 500);
        assert_eq!(checked.max_output_bytes, 100);
        Ok(())
    }

    #[test]
    fn shell_request_rejects_empty_nul_and_too_many_args() {
        let detail = |request: ShellRequest| match check_shell_request(request) {
            Err(DeviceError::Invalid { detail }) => detail,
            _ => String::new(),
        };
        assert_eq!(detail(request(&[])), "empty command");
        assert_eq!(detail(request(&[""])), "invalid argument");
        assert_eq!(detail(request(&["ls", "a\0b"])), "invalid argument");
        let long = "x".repeat(4097);
        assert_eq!(detail(request(&["ls", &long])), "invalid argument");
        let many = ["a"; 65];
        assert_eq!(detail(request(&many)), "too many arguments");
        assert!(check_shell_request(request(&["a"; 64])).is_ok());
    }

    #[test]
    fn cwd_must_stay_in_termux_home_or_be_relative() -> TestResult {
        let with_cwd = |cwd: &str| {
            let mut req = request(&["ls"]);
            req.cwd = Some(cwd.to_owned());
            check_shell_request(req)
        };
        let outside = DeviceError::InvalidPath {
            detail: "working directory outside Termux home".to_owned(),
        };
        let home = "/data/data/com.termux/files/home/project";
        assert_eq!(with_cwd(home)?.cwd.as_deref(), Some(home));
        assert_eq!(with_cwd("src/bin")?.cwd.as_deref(), Some("src/bin"));
        assert_eq!(
            with_cwd("/data/data/com.termux/files/../x").err(),
            Some(outside.clone())
        );
        assert_eq!(
            with_cwd("/data/data/com.termux/files/a\0b").err(),
            Some(outside)
        );
        assert_eq!(
            with_cwd("/etc").err(),
            Some(DeviceError::InvalidPath {
                detail: "absolute path".to_owned()
            })
        );
        assert!(with_cwd("../up").is_err());
        Ok(())
    }

    #[test]
    fn paths_reject_escape_absolute_backslash_nul_and_empty_segments() {
        assert_eq!(normalize_device_path("..".into()), path_err("dot segment"));
        assert_eq!(
            normalize_device_path("a/../b".into()),
            path_err("dot segment")
        );
        assert_eq!(normalize_device_path("./a".into()), path_err("dot segment"));
        assert_eq!(normalize_device_path("/a".into()), path_err("absolute path"));
        assert_eq!(normalize_device_path("a\\b".into()), path_err("backslash"));
        assert_eq!(
            normalize_device_path("a//b".into()),
            path_err("empty segment")
        );
        assert_eq!(normalize_device_path("a\0b".into()), path_err("nul byte"));
        assert_eq!(
            normalize_device_path("x".repeat(256)),
            path_err("segment too long")
        );
        assert_eq!(
            normalize_device_path("a/".repeat(600)),
            path_err("path too long")
        );
    }

    #[test]
    fn paths_normalize_trailing_slash_and_root() {
        assert_eq!(normalize_device_path(String::new()), Ok(String::new()));
        assert_eq!(normalize_device_path("/".into()), path_err("absolute path"));
        assert_eq!(normalize_device_path("a/b/".into()), Ok("a/b".to_owned()));
        assert_eq!(normalize_device_path("a".into()), Ok("a".to_owned()));
        assert_eq!(normalize_device_path("a//".into()), path_err("empty segment"));
    }

    #[test]
    fn callback_failure_maps_to_failed_without_payload() {
        let error =
            DeviceError::from(uniffi::UnexpectedUniFFICallbackError::new("secret-payload"));
        assert_eq!(
            error,
            DeviceError::Failed {
                detail: "unexpected platform failure".to_owned()
            }
        );
        assert!(!error.to_string().contains("secret-payload"));
    }
}

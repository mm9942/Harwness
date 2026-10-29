//! `harw-cloudctl` — local cloud gateway control and TCP status probe.
//!
//! `up`, `down`, and `restart` require an explicit `--service NAME` and act
//! through the current user's supported service manager. `status` reports TCP
//! reachability only; it does not establish service identity or readiness.
//! Device enrollment and revocation are not service lifecycle operations.

use harw_cloud_gateway::config::{GatewayConfig, PortMap};
use harw_cloud_gateway::status::{self as gateway_status};
use harw_install::platform::Platform;
use harw_install::service::ServiceManager;
use serde_json::json;

/// Exit code for invalid command-line usage.
const EXIT_USAGE: i32 = 2;
/// Exit code for unavailable operations or service-manager failures.
const EXIT_FAILURE: i32 = 1;

/// Build the default gateway configuration (127.0.0.1:7443 with ports
/// 7443 `api` and 8080 `enrollment`).
fn default_gateway_config() -> GatewayConfig {
    GatewayConfig {
        listen_addr: "127.0.0.1:7443".into(),
        tls: None,
        ports: vec![
            PortMap {
                name: "api".into(),
                port: 7443,
            },
            PortMap {
                name: "enrollment".into(),
                port: 8080,
            },
        ],
    }
}

/// Print usage to stderr.
fn print_usage() {
    eprintln!(
        "Usage: harw-cloudctl <up|down|restart> --service NAME\n       harw-cloudctl <status|enroll|revoke>"
    );
}

/// Probe the gateway ports and return observations as JSON.
fn probe_status() -> Result<serde_json::Value, String> {
    let config = default_gateway_config();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return Err(format!("could not initialize status probe: {error}")),
    };
    let status = runtime.block_on(gateway_status::probe(&config));
    let ports: Vec<serde_json::Value> = status
        .ports
        .iter()
        .map(|p| json!({ "name": p.name, "port": p.port, "listening": p.listening }))
        .collect();
    Ok(json!({
        "schema_version": 2,
        "any_port_listening": status.any_port_listening,
        "running": status.running,
        "ready": status.ready,
        "ports": ports,
    }))
}

fn unavailable_status(error: &str) -> serde_json::Value {
    json!({
        "schema_version": 2,
        "any_port_listening": null,
        "running": null,
        "ready": null,
        "ports": [],
        "error": error,
    })
}

/// Parse the required explicit service selection for a lifecycle command.
fn parse_service_name(args: &[String]) -> Result<&str, String> {
    if args.len() != 2 || args[0] != "--service" {
        return Err("expected exactly `--service NAME`".to_owned());
    }
    let name = args[1].as_str();
    if name.is_empty() || !valid_service_identifier(name) {
        return Err(format!("invalid service name: {name:?}"));
    }
    Ok(name)
}

fn valid_service_identifier(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'@' | b'-'))
}

/// Dispatch a parsed command through the supplied service manager.
///
/// This takes a manager reference so callers and tests can control which
/// platform backend receives the typed lifecycle operation.
fn dispatch_with_manager(args: &[String], manager: &dyn ServiceManager) -> i32 {
    dispatch_with_status_probe(args, manager, probe_status)
}

fn dispatch_with_status_probe(
    args: &[String],
    manager: &dyn ServiceManager,
    mut status_probe: impl FnMut() -> Result<serde_json::Value, String>,
) -> i32 {
    let Some(subcommand) = args.first().map(String::as_str) else {
        print_usage();
        return EXIT_USAGE;
    };
    let operands = &args[1..];
    match subcommand {
        "up" | "down" | "restart" => {
            let name = match parse_service_name(operands) {
                Ok(name) => name,
                Err(error) => {
                    eprintln!("{error}");
                    print_usage();
                    return EXIT_USAGE;
                }
            };
            let operation = match subcommand {
                "up" => manager.start(name),
                "down" => manager.stop(name),
                "restart" => manager.restart(name),
                _ => return EXIT_USAGE,
            };
            match operation {
                Ok(()) => {
                    println!(
                        "service {name}: {subcommand} requested via {:?}",
                        manager.kind()
                    );
                    0
                }
                Err(error) => {
                    eprintln!("service {name} {subcommand} failed: {error}");
                    EXIT_FAILURE
                }
            }
        }
        "status" if operands.is_empty() => match status_probe() {
            Ok(status) => {
                println!("{status}");
                0
            }
            Err(error) => {
                eprintln!("{error}");
                println!("{}", unavailable_status(&error));
                EXIT_FAILURE
            }
        },
        "enroll" | "revoke" if operands.is_empty() => {
            eprintln!(
                "{subcommand} is unavailable: device enrollment and revocation require a pairing workflow, not service enablement"
            );
            EXIT_FAILURE
        }
        "status" | "enroll" | "revoke" => {
            print_usage();
            EXIT_USAGE
        }
        _ => {
            print_usage();
            EXIT_USAGE
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let platform = Platform::detect();
    let manager = harw_install::service::detect_service_manager(&platform);
    std::process::exit(dispatch_with_manager(&args, manager.as_ref()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_install::service::{ServiceKind, ServiceSpec, ServiceStatus};
    use std::cell::RefCell;

    struct FakeServiceManager {
        calls: RefCell<Vec<(&'static str, String)>>,
        fail: bool,
    }

    impl FakeServiceManager {
        fn new(fail: bool) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                fail,
            }
        }

        fn record(
            &self,
            action: &'static str,
            name: &str,
        ) -> Result<(), harw_install::error::ServiceError> {
            self.calls.borrow_mut().push((action, name.to_owned()));
            if self.fail {
                Err(harw_install::error::ServiceError::Unsupported {
                    detail: "test manager failure".to_owned(),
                })
            } else {
                Ok(())
            }
        }
    }

    impl ServiceManager for FakeServiceManager {
        fn kind(&self) -> ServiceKind {
            ServiceKind::Systemd
        }
        fn render_unit(&self, _spec: &ServiceSpec) -> String {
            String::new()
        }
        fn install(&self, _spec: &ServiceSpec) -> Result<(), harw_install::error::ServiceError> {
            Ok(())
        }
        fn status(&self, _name: &str) -> Result<ServiceStatus, harw_install::error::ServiceError> {
            Ok(ServiceStatus::Stopped)
        }
        fn uninstall(&self, _name: &str) -> Result<(), harw_install::error::ServiceError> {
            Ok(())
        }
        fn start(&self, name: &str) -> Result<(), harw_install::error::ServiceError> {
            self.record("start", name)
        }
        fn stop(&self, name: &str) -> Result<(), harw_install::error::ServiceError> {
            self.record("stop", name)
        }
        fn restart(&self, name: &str) -> Result<(), harw_install::error::ServiceError> {
            self.record("restart", name)
        }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    #[test]
    fn lifecycle_commands_call_the_typed_manager_with_the_explicit_name() {
        for (subcommand, expected) in [("up", "start"), ("down", "stop"), ("restart", "restart")] {
            let manager = FakeServiceManager::new(false);
            let args = argv(&[subcommand, "--service", "harw.gateway"]);
            assert_eq!(dispatch_with_manager(&args, &manager), 0);
            assert_eq!(
                *manager.calls.borrow(),
                vec![(expected, "harw.gateway".to_owned())]
            );
        }
    }

    #[test]
    fn missing_or_invalid_service_names_do_not_call_the_manager() {
        for args in [
            argv(&["up"]),
            argv(&["down", "--service"]),
            argv(&["restart", "--service", "--help"]),
            argv(&["up", "--service", "../gateway"]),
            argv(&["up", "--service", "harw\nservice"]),
            argv(&["up", "--service", "harw\u{1b}service"]),
            argv(&["up", "--service", "harw\0service"]),
            argv(&["up", "--service", "harw", "extra"]),
        ] {
            let manager = FakeServiceManager::new(false);
            assert_eq!(dispatch_with_manager(&args, &manager), EXIT_USAGE);
            assert!(manager.calls.borrow().is_empty());
        }
    }

    #[test]
    fn manager_failure_returns_nonzero_after_one_typed_call() {
        let manager = FakeServiceManager::new(true);
        assert_eq!(
            dispatch_with_manager(&argv(&["up", "--service", "harw"]), &manager),
            EXIT_FAILURE
        );
        assert_eq!(*manager.calls.borrow(), vec![("start", "harw".to_owned())]);
    }

    #[test]
    fn enrollment_commands_are_refused_without_service_calls() {
        for command in ["enroll", "revoke"] {
            let manager = FakeServiceManager::new(false);
            assert_eq!(
                dispatch_with_manager(&argv(&[command]), &manager),
                EXIT_FAILURE
            );
            assert!(manager.calls.borrow().is_empty());
        }
    }

    #[test]
    fn default_gateway_config_has_expected_ports() {
        let config = default_gateway_config();
        assert_eq!(config.listen_addr, "127.0.0.1:7443");
        assert_eq!(config.ports.len(), 2);
        assert_eq!(config.ports[0].name, "api");
        assert_eq!(config.ports[0].port, 7443);
        assert_eq!(config.ports[1].name, "enrollment");
        assert_eq!(config.ports[1].port, 8080);
    }

    #[test]
    fn status_has_no_service_manager_fields() {
        let value = unavailable_status("test probe failure");
        assert_eq!(value["schema_version"], 2);
        assert!(value["running"].is_null());
        assert!(value["ready"].is_null());
        assert!(value["any_port_listening"].is_null());
    }

    #[test]
    fn status_probe_initialization_failure_returns_nonzero_without_service_calls() {
        let manager = FakeServiceManager::new(false);
        let code = dispatch_with_status_probe(&argv(&["status"]), &manager, || {
            Err("runtime initialization failed".to_owned())
        });
        assert_eq!(code, EXIT_FAILURE);
        assert!(manager.calls.borrow().is_empty());
    }
}

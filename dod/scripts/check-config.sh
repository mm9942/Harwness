#!/bin/sh
set -eu

config="${1:-${CONFIG:-/etc/harw-dod/config.toml}}"
if [ "${1:-}" = "--config" ]; then
    config="${2:?--config requires a path}"
fi

if [ ! -f "$config" ]; then
    printf '%s\n' "DoD config not found: $config" >&2
    printf '%s\n' "Install/copy config.toml.example to config.toml and select one active_profile." >&2
    exit 2
fi

# The config crate is the authoritative privileged reader. Until its CLI is
# available, this read-only checker validates the same v1 contract using the
# Python standard library. It never writes, resolves a fallback, or changes a
# host setting.
exec python3 - "$config" <<'PY'
import hashlib
import ipaddress
import pathlib
import sys

try:
    import tomllib
except ImportError:
    print("check-config requires Python 3.11+ (tomllib)", file=sys.stderr)
    raise SystemExit(2)

path = pathlib.Path(sys.argv[1])
raw = path.read_bytes()
try:
    doc = tomllib.loads(raw.decode("utf-8"))
except Exception as exc:
    print(f"invalid DoD TOML {path}: {exc}", file=sys.stderr)
    raise SystemExit(2)

def fail(message):
    print(f"invalid DoD config {path}: {message}", file=sys.stderr)
    raise SystemExit(2)

if not isinstance(doc, dict):
    fail("document must be a table")
allowed_top = {"schema_version", "mode", "active_profile", "profiles"}
unknown = set(doc) - allowed_top
if unknown:
    fail("unknown top-level field(s): " + ", ".join(sorted(unknown)))
if doc.get("schema_version") != 1:
    fail("schema_version must be 1")
if doc.get("mode") != "observe":
    fail("mode must be 'observe'")
profiles = doc.get("profiles")
if not isinstance(profiles, dict) or not profiles:
    fail("profiles must be a non-empty table")
active = doc.get("active_profile")
if not isinstance(active, str) or not active:
    fail("exactly one non-empty active_profile is required before enable")
if active not in profiles:
    fail(f"active_profile {active!r} is not defined")

def valid_profile_name(value):
    if not isinstance(value, str) or not value:
        return False
    for index, char in enumerate(value):
        if index > 0 and char == ".":
            continue
        if not (ord(char) < 128 and (char.isalnum() or char in "_-")):
            return False
    return True

for name, profile in profiles.items():
    # Keep this fallback checker byte-for-byte compatible with ProfileId::parse
    # in harw-dod-config. The privileged binaries remain authoritative, but
    # enable/restart must not accept a shape that they will reject later.
    if not valid_profile_name(name):
        fail("profile names must contain only ASCII letters, digits, '_', '-', or '.' (not leading '.')")
    if not isinstance(profile, dict):
        fail(f"profile {name!r} must be a table")
    allowed = {"scope", "cgroup_paths", "include_descendants", "sensors", "egress_allow_cidrs"}
    unknown = set(profile) - allowed
    if unknown:
        fail(f"profile {name!r} has unknown field(s): " + ", ".join(sorted(unknown)))
    scope = profile.get("scope")
    if scope not in ("cgroups", "host"):
        fail(f"profile {name!r} scope must be 'cgroups' or 'host'")
    sensors = profile.get("sensors")
    if not isinstance(sensors, list) or not sensors:
        fail(f"profile {name!r} sensors must be a non-empty list")
    if any(sensor not in ("exec", "tcp-connect") for sensor in sensors):
        fail(f"profile {name!r} contains an unknown sensor")
    if len(set(sensors)) != len(sensors):
        fail(f"profile {name!r} repeats a sensor")
    cidrs = profile.get("egress_allow_cidrs", [])
    if not isinstance(cidrs, list):
        fail(f"profile {name!r} egress_allow_cidrs must be a list")
    for cidr in cidrs:
        try:
            ipaddress.ip_network(cidr, strict=True)
        except Exception as exc:
            fail(f"profile {name!r} has invalid CIDR {cidr!r}: {exc}")
    if len(set(cidrs)) != len(cidrs):
        fail(f"profile {name!r} repeats an egress_allow_cidrs entry")
    if scope == "cgroups":
        paths = profile.get("cgroup_paths")
        if not isinstance(paths, list) or not paths:
            fail(f"profile {name!r} cgroups scope requires cgroup_paths")
        if profile.get("include_descendants") not in (True, False):
            fail(f"profile {name!r} cgroups scope requires include_descendants")
        if any(not isinstance(path, str) for path in paths):
            fail(f"profile {name!r} cgroup paths must be strings")
        if len(set(paths)) != len(paths):
            fail(f"profile {name!r} repeats a cgroup path")
        for cgroup in paths:
            if not isinstance(cgroup, str) or not cgroup.startswith("/"):
                fail(f"profile {name!r} cgroup paths must begin with '/'")
            if cgroup != "/" and cgroup.endswith("/"):
                fail(f"profile {name!r} cgroup paths must not end with '/'")
            if cgroup != "/" and any(
                part in ("", ".", "..") or any(ord(char) < 0x20 for char in part)
                for part in cgroup.split("/")[1:]
            ):
                fail(f"profile {name!r} cgroup path contains traversal or an empty segment")
    else:
        if "cgroup_paths" in profile or "include_descendants" in profile:
            fail(f"profile {name!r} host scope cannot contain cgroup fields")

digest = hashlib.sha256(raw).hexdigest()
selected = profiles[active]
print(f"config={path}")
print(f"digest=sha256:{digest}")
print(f"active_profile={active}")
print(f"scope={selected['scope']}")
print("sensors=" + ",".join(selected["sensors"]))
if selected["scope"] == "cgroups":
    print("cgroup_paths=" + ",".join(selected["cgroup_paths"]))
else:
    print("cgroup_paths=<host>")
PY

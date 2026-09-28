#!/bin/sh
set -eu

# Manifest-driven installer.  This script is intentionally independent of
# Cargo: packaging must never compile as root and DESTDIR staging must be
# safe to run in a package build sandbox.
action=${1:-install}

prefix=${PREFIX:-/usr/local}
destdir=${DESTDIR:-}
libexecdir=${LIBEXECDIR:-$prefix/libexec/harw-dod}
libdir=${LIBDIR:-$prefix/lib/harw-dod}
bpfdir=${BPFDIR:-$libdir/bpf}
sysconfdir=${SYSCONFDIR:-/etc/harw-dod}
statedir=${STATEDIR:-${LOCALSTATEDIR:-/var}/lib/harw-dod}
logdir=${LOGDIR:-${LOCALSTATEDIR:-/var}/log/harw-dod}
unitdir=${SYSTEMD_UNITDIR:-$prefix/lib/systemd/system}
sysusersdir=${SYSUSERSDIR:-$prefix/lib/sysusers.d}
tmpfilesdir=${TMPFILESDIR:-$prefix/lib/tmpfiles.d}
binary_dir=${BINARY_DIR:-$(CDPATH= cd -- "$(dirname "$0")/../target/release" && pwd)}
bpf_artifact_dir=${BPF_ARTIFACT_DIR:-$(CDPATH= cd -- "$(dirname "$0")/../bpf" && pwd)}
manifest=$(CDPATH= cd -- "$(dirname "$0")/../packaging" && pwd)/manifest
packaging_dir=$(CDPATH= cd -- "$(dirname "$0")/../packaging" && pwd)
# The one canonical source of every unit, sysusers and tmpfiles snippet
# (Crypto Masterplan v2 §22, H10): deploy/ at the repository root. The same
# files are embedded byte-identically into harw-install
# (`harw install --print-systemd`); there is no second copy under dod/.
deploy_dir=${DEPLOY_DIR:-$(CDPATH= cd -- "$(dirname "$0")/../../deploy" && pwd)}

# DoD units installed from $deploy_dir/systemd. The infrastructure units
# (harw-infra.target, harw-auth-hub.*, ...) in the same directory are not
# part of the DoD package.
dod_units='harw-dod.target harw-sentinel.service harw-probe-bpf.service harw-probe-fs.service harw-warden.service harw-warden.socket'
# Files of the pre-H10 layout (the old dod/packaging unit, sysusers and tmpfiles trees). Removed on
# install and uninstall so an upgraded host never keeps two unit sets.
legacy_files='systemd/harw-dod-sentinel.service systemd/harw-dod-bpf.service systemd/harw-dod-warden.service systemd/harw-dod-warden.socket sysusers/harw-dod.conf tmpfiles/harw-dod.conf'

root_path() {
	path=$1
	if [ -n "$destdir" ]; then
		printf '%s%s\n' "${destdir%/}" "$path"
	else
		printf '%s\n' "$path"
	fi
}

fail() { printf 'DoD install: %s\n' "$*" >&2; exit 2; }

require_root_for_host_install() {
	[ -n "$destdir" ] && return 0
	[ "$(id -u)" -eq 0 ] || fail 'host installation requires root; use DESTDIR for unprivileged staging'
}

ensure_dir() {
	mode=$1
	path=$2
	install -d -m "$mode" "$(root_path "$path")"
}

check_artifacts() {
	for binary in harw-sentinel harw-probe-bpf harw-probe-fs harw-warden; do
		[ -f "$binary_dir/$binary" ] || fail "missing $binary_dir/$binary; build release binaries before install (do not build as root)"
		test -x "$binary_dir/$binary" || fail "binary is not executable: $binary_dir/$binary"
	done
	for object in exec.bpf.o exit.bpf.o tcp_v4_connect.bpf.o tcp_v6_connect.bpf.o manifest.json; do
		[ -f "$bpf_artifact_dir/$object" ] || fail "missing $bpf_artifact_dir/$object; build pinned BPF artifacts before install"
	done
}

render_unit() {
	source=$1
	destination=$2
	tmp=${destination}.tmp.$$
	sed \
		-e "s|@LIBEXECDIR@|$libexecdir|g" \
		-e "s|@BPFDIR@|$bpfdir|g" \
		-e "s|@SYSCONFDIR@|$sysconfdir|g" \
		-e "s|@STATEDIR@|$statedir|g" \
		-e "s|@LOGDIR@|$logdir|g" \
		"$source" > "$tmp"
	# Masterplan §40: no unresolved @PLACEHOLDER@ may reach the host.
	if grep -n '@[A-Z][A-Z_]*@' "$tmp" >&2; then
		rm -f "$tmp"
		fail "unresolved placeholder in rendered $source"
	fi
	install -m 0644 "$tmp" "$destination"
	rm -f "$tmp"
}

install_file() {
	mode=$1
	source=$2
	destination=$3
	[ -f "$source" ] || fail "packaging source missing: $source"
	install -m "$mode" "$source" "$(root_path "$destination")"
}

legacy_destination() {
	case "$1" in
		systemd/*) printf '%s\n' "$unitdir/${1#systemd/}" ;;
		sysusers/*) printf '%s\n' "$sysusersdir/${1#sysusers/}" ;;
		tmpfiles/*) printf '%s\n' "$tmpfilesdir/${1#tmpfiles/}" ;;
		*) fail "unknown legacy entry: $1" ;;
	esac
}

remove_legacy_files() {
	for entry in $legacy_files; do
		destination=$(root_path "$(legacy_destination "$entry")")
		if [ -e "$destination" ]; then
			rm -f "$destination"
			printf '%s\n' "Removed pre-H10 file: $destination"
		fi
	done
}

install_package() {
	require_root_for_host_install
	check_artifacts

	# Create only package-owned directories.  The persistent config itself is
	# deliberately not part of this list and is never overwritten.
	ensure_dir 0755 "$libexecdir"
	ensure_dir 0755 "$bpfdir"
	ensure_dir 0755 "$unitdir"
	ensure_dir 0755 "$sysusersdir"
	ensure_dir 0755 "$tmpfilesdir"
	ensure_dir 0750 "$sysconfdir"
	ensure_dir 0750 "$statedir"
	ensure_dir 0750 "$logdir"

	for binary in harw-sentinel harw-probe-bpf harw-probe-fs harw-warden; do
		install_file 0755 "$binary_dir/$binary" "$libexecdir/$binary"
	done
	for object in exec.bpf.o exit.bpf.o tcp_v4_connect.bpf.o tcp_v6_connect.bpf.o manifest.json; do
		install_file 0644 "$bpf_artifact_dir/$object" "$bpfdir/$object"
	done

	remove_legacy_files
	for unit in $dod_units; do
		[ -f "$deploy_dir/systemd/$unit" ] || fail "deployment source missing: $deploy_dir/systemd/$unit"
		destination=$(root_path "$unitdir/$unit")
		render_unit "$deploy_dir/systemd/$unit" "$destination"
	done
	install_file 0644 "$deploy_dir/sysusers.d/harw.conf" "$sysusersdir/harw.conf"
	[ -f "$deploy_dir/tmpfiles.d/harw.conf" ] || fail "deployment source missing: $deploy_dir/tmpfiles.d/harw.conf"
	render_unit "$deploy_dir/tmpfiles.d/harw.conf" "$(root_path "$tmpfilesdir/harw.conf")"
	install_file 0640 "$packaging_dir/config.example.toml" "$sysconfdir/config.toml.example"

	if [ -z "$destdir" ]; then
		command -v systemd-sysusers >/dev/null 2>&1 || fail 'systemd-sysusers is required for host installation'
		command -v systemd-tmpfiles >/dev/null 2>&1 || fail 'systemd-tmpfiles is required for host installation'
		systemd-sysusers "$(root_path "$sysusersdir/harw.conf")"
		# The sentinel needs read access, while root retains ownership and
		# write control.  Existing config contents are never replaced.
		chown root:harw-dod-config "$(root_path "$sysconfdir")"
		chown root:harw-dod-config "$(root_path "$sysconfdir/config.toml.example")"
		if [ -f "$(root_path "$sysconfdir/config.toml")" ]; then
			chown root:harw-dod-config "$(root_path "$sysconfdir/config.toml")"
		fi
		systemd-tmpfiles --create "$(root_path "$tmpfilesdir/harw.conf")"
		command -v systemctl >/dev/null 2>&1 || fail 'systemctl is required for host installation'
		systemctl daemon-reload
	fi

	if [ -f "$(root_path "$sysconfdir/config.toml")" ]; then
		printf '%s\n' "Preserved existing config: $(root_path "$sysconfdir/config.toml")"
	else
		printf '%s\n' "Installed example only; copy it to $(root_path "$sysconfdir/config.toml") and choose active_profile before enable."
	fi
	printf '%s\n' "DoD package installed${destdir:+ below $destdir}; services were not started or enabled."
}

uninstall_package() {
	require_root_for_host_install
	[ -f "$manifest" ] || fail "manifest missing: $manifest"

	while IFS= read -r entry || [ -n "$entry" ]; do
		case "$entry" in
			''|'#'*) continue ;;
		esac
		case "$entry" in
			libexec/*) destination=$libexecdir/${entry#libexec/} ;;
			bpf/*) destination=$bpfdir/${entry#bpf/} ;;
			systemd/*) destination=$unitdir/${entry#systemd/} ;;
			sysusers/*) destination=$sysusersdir/${entry#sysusers/} ;;
			tmpfiles/*) destination=$tmpfilesdir/${entry#tmpfiles/} ;;
			config/config.toml.example) destination=$sysconfdir/config.toml.example ;;
			*) fail "unknown manifest entry: $entry" ;;
		esac
		rm -f "$(root_path "$destination")"
	done < "$manifest"
	remove_legacy_files

	if [ -z "$destdir" ]; then
		command -v systemctl >/dev/null 2>&1 && systemctl daemon-reload || true
	fi
	printf '%s\n' 'Removed manifest-owned DoD files; configuration, state, logs, runtime data, and accounts were preserved.'
}

case "$action" in
	install) install_package ;;
	uninstall) uninstall_package ;;
	*) fail "usage: install.sh [install|uninstall]" ;;
esac

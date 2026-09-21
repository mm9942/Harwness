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
runtimedir=${RUNTIMEDIR:-${RUNSTATEDIR:-/run}/harw-dod}
unitdir=${SYSTEMD_UNITDIR:-$prefix/lib/systemd/system}
sysusersdir=${SYSUSERSDIR:-$prefix/lib/sysusers.d}
tmpfilesdir=${TMPFILESDIR:-$prefix/lib/tmpfiles.d}
binary_dir=${BINARY_DIR:-$(CDPATH= cd -- "$(dirname "$0")/../target/release" && pwd)}
bpf_artifact_dir=${BPF_ARTIFACT_DIR:-$(CDPATH= cd -- "$(dirname "$0")/../bpf" && pwd)}
manifest=$(CDPATH= cd -- "$(dirname "$0")/../packaging" && pwd)/manifest
packaging_dir=$(CDPATH= cd -- "$(dirname "$0")/../packaging" && pwd)

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
		-e "s|@RUNTIMEDIR@|$runtimedir|g" \
		"$source" > "$tmp"
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
	ensure_dir 0750 "$runtimedir"

	for binary in harw-sentinel harw-probe-bpf harw-probe-fs harw-warden; do
		install_file 0755 "$binary_dir/$binary" "$libexecdir/$binary"
	done
	for object in exec.bpf.o exit.bpf.o tcp_v4_connect.bpf.o tcp_v6_connect.bpf.o manifest.json; do
		install_file 0644 "$bpf_artifact_dir/$object" "$bpfdir/$object"
	done

	for unit in harw-dod.target harw-dod-sentinel.service harw-dod-bpf.service harw-dod-warden.service harw-dod-warden.socket; do
		destination=$(root_path "$unitdir/$unit")
		render_unit "$packaging_dir/systemd/$unit" "$destination"
	done
	install_file 0644 "$packaging_dir/sysusers.d/harw-dod.conf" "$sysusersdir/harw-dod.conf"
	render_unit "$packaging_dir/tmpfiles.d/harw-dod.conf" "$(root_path "$tmpfilesdir/harw-dod.conf")"
	install_file 0640 "$packaging_dir/config.example.toml" "$sysconfdir/config.toml.example"

	if [ -z "$destdir" ]; then
		command -v systemd-sysusers >/dev/null 2>&1 || fail 'systemd-sysusers is required for host installation'
		command -v systemd-tmpfiles >/dev/null 2>&1 || fail 'systemd-tmpfiles is required for host installation'
		systemd-sysusers "$(root_path "$sysusersdir/harw-dod.conf")"
		# The sentinel needs read access, while root retains ownership and
		# write control.  Existing config contents are never replaced.
		chown root:harw-dod-config "$(root_path "$sysconfdir")"
		chown root:harw-dod-config "$(root_path "$sysconfdir/config.toml.example")"
		if [ -f "$(root_path "$sysconfdir/config.toml")" ]; then
			chown root:harw-dod-config "$(root_path "$sysconfdir/config.toml")"
		fi
		systemd-tmpfiles --create "$(root_path "$tmpfilesdir/harw-dod.conf")"
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

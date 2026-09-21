#!/bin/sh
# ensure-bpf-toolchain.sh — locate (and, when explicitly allowed, install)
# the three external tools the standalone C BPF build domain (dod/bpf/)
# needs: clang, bpftool, llvm-objdump.  This lives in the dod Makefile's
# domain, not in dod/bpf/, because installing dependencies is deliberately
# the dod package Makefile's job; dod/bpf/Makefile has no dependency-install
# target by design and stays a self-contained build description.
#
# This script never checks *versions*.  Version pinning stays exclusively
# the job of dod/bpf/scripts/verify-toolchain.sh against
# dod/bpf/toolchain.lock.toml; an apt-installed tool that does not match the
# lock still fails the build there, fail-closed, exactly as before.
#
# Modes:
#   (no args)            Search, then install any missing tool via apt-get
#                         if allowed (see below); print "<tool>=<path>" for
#                         clang, bpftool, llvm-objdump; exit 2 if any tool is
#                         still missing afterwards.
#   --check               Read-only: search only, never installs. Prints the
#                         same "<tool>=<path>" lines ("<tool>=MISSING" when
#                         not found) and exits 1 if anything is missing.
#                         Used by `make doctor`, which stays read-only.
#   --print-var <tool>    Read-only: search only, print just the resolved
#                         absolute path for one tool (clang | bpftool |
#                         llvm-objdump). Falls back to printing the bare
#                         command name when not found, so a Makefile
#                         `$(shell ...)` call always yields a usable value
#                         and never blocks on a missing tool by itself.
#                         Always exits 0.
#
# Search order per tool: PATH first (via `command -v`), then /usr/sbin and
# /sbin (Debian's bpftool package installs there, which is commonly outside
# an unprivileged user's PATH). llvm-objdump is also looked up under the
# versioned name llvm-objdump-19 in each of those locations.
#
# Installation (default mode only, when a tool is genuinely missing):
# determines the Debian package(s) for the missing tool(s) and runs
# `$PRIV_ESC apt-get install -y <packages>`. PRIV_ESC is passed in from the
# calling Makefile (already resolved to "sudo -E" or empty there). Install is
# refused — with exit 2 and a message naming the exact command to run by
# hand — whenever any of these hold: DESTDIR is set (a staging/packaging
# build must never touch the host), there is no PRIV_ESC and the caller is
# not root, or apt-get is not present. Nothing is ever downloaded; only
# `apt-get install` against the configured apt sources is used.
set -eu

mode=${1:-}
priv_esc=${PRIV_ESC:-}
destdir=${DESTDIR:-}

# Search PATH, then /usr/sbin and /sbin, for $1 (the default command name).
# Prints the absolute path and returns 0 on success; returns 1 if not found.
search_one() {
	name=$1
	if command -v "$name" >/dev/null 2>&1; then
		command -v "$name"
		return 0
	fi
	for dir in /usr/sbin /sbin; do
		if [ -x "$dir/$name" ]; then
			printf '%s/%s\n' "$dir" "$name"
			return 0
		fi
	done
	return 1
}

# Resolve one logical tool ("clang", "bpftool", "llvm-objdump") to an
# absolute path using search_one, plus the llvm-objdump-19 alias for
# llvm-objdump. Prints the path and returns 0, or returns 1 if not found.
resolve_tool() {
	tool=$1
	case "$tool" in
	clang) search_one clang && return 0 ;;
	bpftool) search_one bpftool && return 0 ;;
	llvm-objdump)
		search_one llvm-objdump && return 0
		search_one llvm-objdump-19 && return 0
		;;
	*)
		printf 'ensure-bpf-toolchain: unknown tool %s\n' "$tool" >&2
		exit 2
		;;
	esac
	return 1
}

# Map a logical tool name to the Debian package that provides it. clang
# prefers the exact pinned major version (clang-19) when apt knows about it,
# and otherwise falls back to the generic clang metapackage.
pkg_for_tool() {
	tool=$1
	case "$tool" in
	clang)
		if command -v apt-cache >/dev/null 2>&1 && apt-cache show clang-19 >/dev/null 2>&1; then
			printf 'clang-19\n'
		else
			printf 'clang\n'
		fi
		;;
	bpftool) printf 'bpftool\n' ;;
	llvm-objdump) printf 'llvm-19\n' ;;
	*)
		printf 'ensure-bpf-toolchain: unknown tool %s\n' "$tool" >&2
		exit 2
		;;
	esac
}

tools='clang bpftool llvm-objdump'

case "$mode" in
--print-var)
	tool=${2:-}
	case "$tool" in
	clang | bpftool | llvm-objdump) ;;
	*)
		printf 'ensure-bpf-toolchain: --print-var requires one of clang, bpftool, llvm-objdump\n' >&2
		exit 2
		;;
	esac
	if path=$(resolve_tool "$tool"); then
		printf '%s\n' "$path"
	else
		# Fall back to the bare command name so a Makefile default
		# (e.g. CLANG ?= clang) still gets a usable value; the real
		# failure, if any, surfaces via verify-toolchain.sh or the
		# unprivileged build itself.
		case "$tool" in
		llvm-objdump) printf 'llvm-objdump\n' ;;
		*) printf '%s\n' "$tool" ;;
		esac
	fi
	exit 0
	;;
--check)
	status=0
	for tool in $tools; do
		if path=$(resolve_tool "$tool"); then
			printf '%s=%s\n' "$tool" "$path"
		else
			printf '%s=MISSING\n' "$tool"
			status=1
		fi
	done
	exit "$status"
	;;
'')
	# Default mode: search, install what is genuinely missing (if
	# allowed), then report resolved paths.
	;;
*)
	printf 'ensure-bpf-toolchain: unknown mode %s (expected --check or --print-var)\n' "$mode" >&2
	exit 2
	;;
esac

missing=''
for tool in $tools; do
	if ! resolve_tool "$tool" >/dev/null; then
		missing="$missing $tool"
	fi
done

if [ -n "$missing" ]; then
	can_install=1
	reason=''
	if [ -n "$destdir" ]; then
		can_install=0
		reason='DESTDIR is set (staging build must never install onto the host)'
	elif [ "$(id -u)" -ne 0 ] && [ -z "$priv_esc" ]; then
		can_install=0
		reason='not root and no PRIV_ESC available'
	elif ! command -v apt-get >/dev/null 2>&1; then
		can_install=0
		reason='apt-get is not available'
	fi

	pkgs=''
	# shellcheck disable=SC2086 # $missing is a deliberate word list
	for tool in $missing; do
		pkg=$(pkg_for_tool "$tool")
		pkgs="$pkgs $pkg"
	done
	# shellcheck disable=SC2086 # $pkgs is a deliberate word list
	set -- $pkgs
	pkgs_joined=$*

	if [ "$can_install" -eq 0 ]; then
		printf 'ensure-bpf-toolchain: missing tool(s):%s\n' "$missing" >&2
		printf 'ensure-bpf-toolchain: cannot install automatically (%s).\n' "$reason" >&2
		printf 'ensure-bpf-toolchain: run this yourself, then retry: sudo apt-get install -y%s\n' " $pkgs_joined" >&2
		exit 2
	fi

	printf 'ensure-bpf-toolchain: installing missing package(s):%s\n' " $pkgs_joined"
	# shellcheck disable=SC2086 # $priv_esc and $pkgs_joined are deliberate word lists
	$priv_esc apt-get install -y $pkgs_joined

	still_missing=''
	for tool in $missing; do
		if ! resolve_tool "$tool" >/dev/null; then
			still_missing="$still_missing $tool"
		fi
	done
	if [ -n "$still_missing" ]; then
		printf 'ensure-bpf-toolchain: still missing after install:%s\n' "$still_missing" >&2
		exit 2
	fi
fi

for tool in $tools; do
	path=$(resolve_tool "$tool")
	printf '%s=%s\n' "$tool" "$path"
done

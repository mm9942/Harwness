#!/bin/sh
set -eu

action=${1:?usage: service.sh enable|disable|restart|status|logs|smoke}
systemctl_bin=${SYSTEMCTL:-systemctl}
config=${CONFIG:-${SYSCONFDIR:-/etc/harw-dod}/config.toml}

require_root() {
    if [ "$(id -u)" -ne 0 ]; then
        printf '%s\n' "service action '$action' requires root (use sudo explicitly)" >&2
        exit 2
    fi
}

case "$action" in
    enable)
        require_root
        "$(dirname "$0")/check-config.sh" --config "$config"
        "$systemctl_bin" daemon-reload
        "$systemctl_bin" enable --now harw-dod.target
        printf '%s\n' 'Observation target enabled and started; Warden remains disabled.'
        ;;
    disable)
        require_root
        "$systemctl_bin" disable --now harw-dod.target harw-sentinel.service harw-probe-bpf.service harw-probe-fs.service
        # Explicitly remove any accidental Warden activation without starting it.
        "$systemctl_bin" disable --now harw-warden.socket harw-warden.service 2>/dev/null || true
        ;;
    restart)
        require_root
        "$(dirname "$0")/check-config.sh" --config "$config"
        "$systemctl_bin" daemon-reload
        "$systemctl_bin" restart harw-dod.target
        ;;
    status)
        "$(dirname "$0")/check-config.sh" --config "$config" || true
        "$systemctl_bin" --no-pager status harw-dod.target harw-sentinel.service harw-probe-bpf.service || true
        "$systemctl_bin" --no-pager is-enabled harw-warden.socket harw-warden.service 2>&1 || true
        ;;
    logs)
        exec journalctl --no-pager -u harw-sentinel.service -u harw-probe-bpf.service -u harw-probe-fs.service -u harw-warden.service
        ;;
    smoke)
        if [ "${SMOKE_CONFIRM:-0}" != 1 ]; then
            printf '%s\n' 'smoke is an explicit live action; rerun with SMOKE_CONFIRM=1 make -C dod smoke' >&2
            exit 2
        fi
        require_root
        "$systemctl_bin" is-active --quiet harw-dod.target
        printf '%s\n' 'Target is active. Run the documented temporary-cgroup/IPv4+IPv6 scenario separately; no firewall or Warden action is performed by this helper.'
        ;;
    *)
        printf '%s\n' "unknown action: $action" >&2
        exit 2
        ;;
esac

#!/usr/bin/env bash
# Run this ON the server 78.47.120.193 (via provider console or an existing login).
# It removes only the CrowdSec decision for the Raspberry Pi's current public IP,
# then prints the relevant SSH, CrowdSec, and firewall diagnostics.
#
# Optional: pass another IP as the first argument if the Pi's public IP changes.
#   sudo ./scripts/repair-raspi-ssh.sh 203.0.113.10

set -u

PI_PUBLIC_IP="${1:-91.37.108.44}"
SSH_PORT=22
SERVER_IP="78.47.120.193"

if [[ ! "$PI_PUBLIC_IP" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]; then
  echo "Ungültige IPv4-Adresse: $PI_PUBLIC_IP" >&2
  exit 2
fi

if [[ "${EUID}" -ne 0 ]]; then
  exec sudo -- "$0" "$@"
fi

run() {
  echo
  echo "==> $*"
  "$@"
  local status=$?
  if (( status != 0 )); then
    echo "    (Befehl endete mit Status $status; weiter mit der Diagnose.)" >&2
  fi
  return 0
}

echo "Raspberry-Pi-SSH-Reparatur für ${SERVER_IP}:${SSH_PORT}"
echo "Betroffene öffentliche Pi-IP: ${PI_PUBLIC_IP}"
echo "Zeitpunkt: $(date -Is)"

# Do not stop CrowdSec or open the firewall globally.  Remove only this IP's
# ban, if one exists.  A missing decision is harmless.
if command -v cscli >/dev/null 2>&1; then
  run cscli decisions list --ip "$PI_PUBLIC_IP"
  run cscli decisions delete --ip "$PI_PUBLIC_IP"
else
  echo
  echo "WARNUNG: cscli ist nicht installiert oder nicht im PATH; CrowdSec konnte nicht geprüft werden." >&2
fi

run systemctl is-active ssh
run systemctl status ssh --no-pager -n 20
run ss -ltnp

echo
echo "==> Lauscht sshd auf TCP-Port ${SSH_PORT}?"
if ss -ltn | awk -v port=":${SSH_PORT}" '$4 ~ port"$" { found=1 } END { exit !found }'; then
  echo "    Ja."
else
  echo "    NEIN: sshd lauscht nicht sichtbar auf Port ${SSH_PORT}." >&2
fi

if command -v ufw >/dev/null 2>&1; then
  run ufw status numbered
fi

if command -v nft >/dev/null 2>&1; then
  echo
  echo "==> nftables-Regeln mit Bezug zur Pi-IP (falls vorhanden)"
  nft list ruleset | grep -C 3 -F "$PI_PUBLIC_IP" || echo "    Keine explizite nftables-Regel für diese IP gefunden."
fi

if command -v iptables >/dev/null 2>&1; then
  echo
  echo "==> iptables-Regeln mit Bezug zur Pi-IP (falls vorhanden)"
  iptables -S | grep -F "$PI_PUBLIC_IP" || echo "    Keine explizite iptables-Regel für diese IP gefunden."
fi

run journalctl -u ssh -n 100 --no-pager

echo
echo "Fertig. Jetzt AUF DEM RASPBERRY PI testen:"
echo "  ssh -vvv -o ConnectTimeout=10 mm29942@${SERVER_IP}"
echo
echo "Falls sich die öffentliche Pi-IP geändert hat, auf dem Pi prüfen:"
echo "  curl -4 https://ifconfig.me"

#!/bin/sh
# Setup: dauerhafte Profil-Agentendefinition "rust-implementer" (Lese- + Schreibrechte, kein Shell/Netz/Spawn).
# Host-Modus noetig, weil der Profil-Scope (~/.harw/profiles/...) ausserhalb des Workspace liegt.
set -eu

HARW_HOME="${HARW_HOME:-$HOME/.harw}"
PROFILE_DIR="$HARW_HOME/profiles/${HARW_PROFILE:-default}"
AGENT_DIR="$PROFILE_DIR/agents/rust-implementer"
DEF="$AGENT_DIR/definition.toml"

[ -d "$PROFILE_DIR/agents" ] || { echo "ERROR: profile agents dir not found: $PROFILE_DIR/agents" >&2; exit 1; }

# Backup bestehender Zustand (falls vorhanden)
if [ -e "$AGENT_DIR" ]; then
  BACKUP="$PROFILE_DIR/agents/rust-implementer.bak.$(date +%Y%m%d-%H%M%S)"
  cp -a "$AGENT_DIR" "$BACKUP"
  echo "BACKUP: $BACKUP"
fi

mkdir -p "$AGENT_DIR"

cat > "$DEF" <<'TOML'
schema = "harwness.agent/v1"
id = "harwness.agent.rust-implementer@1"
version = "1.0.0"
extends = { id = "harwness.agent.worker-base@1" }
role = "worker"
specialization = "rust-implementer"

name = "Rust Implementer"
description = "Rust-Implementer mit Lese- und Schreibrechten im Workspace: Quellarbeit mit fs.read/fs.grep/fs.search, Aenderungen mit fs.edit/fs.write. Kein Shell, kein Netz, keine Agenten-Spawns."

[tools]
admitted = ["fs.read", "fs.list", "fs.search", "fs.glob", "fs.grep", "fs.edit", "fs.write", "doc.read_pdf"]
forbidden = ["shell.exec", "job.start", "job.stop", "process.kill", "agents.delegate", "gateway.channels_connect", "cloud.cloudctl_up", "cloud.cloudctl_down", "cloud.cloudctl_restart", "cloud.cloudctl_enroll", "cloud.cloudctl_revoke", "sandbox-lease", "uia_self_update_document"]

[spawn]
max_depth = 0

[spawn.budget]
max_tokens = 60000
max_tool_calls = 50
max_wall_secs = 300

[return]
contract = "harwness.return.execution-summary@1"
TOML

chmod 600 "$DEF"

# Sicherstellen, dass kein Legacy agent.toml die Runtime startet (known failure mode)
rm -f "$AGENT_DIR/agent.toml"

echo "WRITTEN: $DEF"
ls -l "$DEF"

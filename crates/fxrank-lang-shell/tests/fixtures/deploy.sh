#!/usr/bin/env bash
set -euo pipefail

# A destructive/deploy sample: `deploy` wipes the release dir with a
# privilege-escalated recursive rm, then reinstalls; `rollback` reverts a
# git checkout. Exercises DestructiveFs + PrivilegeEscalation risk detection
# and a top-level call that forces the synthetic `<script>` unit.

deploy() {
  local env="$1"
  sudo rm -rf "/opt/releases/$env"/*
  cp -r ./build "/opt/releases/$env"
  chmod -R 755 "/opt/releases/$env"
}

rollback() {
  git checkout HEAD~1
}

deploy "$1"

#!/usr/bin/env bash
# Install the ESP32-S3 footprint git hooks. Git never version-controls
# .git/hooks, so a fresh clone has the scripts under .claude/hooks/ but nothing
# wired up — run this once after cloning:
#
#     bash .claude/hooks/install.sh
#
# Idempotent: re-running overwrites the three hook files with the current
# wiring. Uses --git-common-dir so it works from the main checkout or any
# linked worktree (all worktrees share one hooks directory).

set -euo pipefail

HOOKS_DIR="$(git rev-parse --git-common-dir)/hooks"
mkdir -p "$HOOKS_DIR"

write_hook() { # name body
  local path="$HOOKS_DIR/$1"
  printf '%s\n' "$2" > "$path"
  chmod +x "$path"
  echo "  installed $1"
}

echo "Installing footprint hooks into $HOOKS_DIR"

# pre-commit: print the footprint (read-only) before the commit is made.
write_hook pre-commit '#!/usr/bin/env bash
bash "$(git rev-parse --show-toplevel)/.claude/hooks/esp32-size.sh" 2>&1 || true'

# prepare-commit-msg: record the footprint row INTO the commit being created
# (runs before git snapshots the index, so the staged row is part of the
# commit — no amend). $1 is the commit message file.
write_hook prepare-commit-msg '#!/usr/bin/env bash
bash "$(git rev-parse --show-toplevel)/.claude/hooks/esp32-size-record.sh" "$@" 2>&1 || true'

# post-commit: currently a no-op (recording moved to prepare-commit-msg).
write_hook post-commit '#!/usr/bin/env bash
bash "$(git rev-parse --show-toplevel)/.claude/hooks/esp32-size-update.sh" 2>&1 || true'

echo "Done."

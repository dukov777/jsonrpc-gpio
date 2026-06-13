#!/usr/bin/env bash
# Append an ESP32-S3 footprint row to MEMORY_LOG.md and stage it so it is part
# of THE COMMIT BEING CREATED. Invoked from the `prepare-commit-msg` hook, which
# runs before git snapshots the tree from the index — so staging here includes
# the row in this same commit, with NO `git commit --amend` (amending from a
# commit hook can scramble history). $1 is the path to the commit message file,
# from which we read the real subject line.

set -euo pipefail

MSGFILE="${1:-}"

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || realpath "$(dirname "$0")/../..")"
ELF_DEBUG="$REPO_ROOT/target/xtensa-esp32s3-espidf/debug/jsonrpc-gpio"
ELF_RELEASE="$REPO_ROOT/target/xtensa-esp32s3-espidf/release/jsonrpc-gpio"
BASELINE="$REPO_ROOT/.claude/esp32-size-baseline"
LOG="$REPO_ROOT/MEMORY_LOG.md"

# Pick the most recently built ELF
ELF=""; PROFILE=""
if [ -f "$ELF_RELEASE" ] && { [ ! -f "$ELF_DEBUG" ] || [ "$ELF_RELEASE" -nt "$ELF_DEBUG" ]; }; then
  ELF="$ELF_RELEASE"; PROFILE="release"
elif [ -f "$ELF_DEBUG" ]; then
  ELF="$ELF_DEBUG"; PROFILE="debug"
else
  exit 0
fi

APP_CRATE="jsonrpc_gpio"

SIZE_TOOL=$(find ~/.espressif/tools/xtensa-esp-elf -name "xtensa-esp32s3-elf-size" 2>/dev/null | sort -V | tail -1)
[ -z "$SIZE_TOOL" ] || [ ! -x "$SIZE_TOOL" ] && exit 0
NM_TOOL=$(find ~/.espressif/tools/xtensa-esp-elf -name "xtensa-esp32s3-elf-nm" 2>/dev/null | sort -V | tail -1)

read -r FLASH DRAM IRAM < <(
  "$SIZE_TOOL" -A "$ELF" 2>/dev/null | awk '
    /^\.flash\.text /    { ft=$2 } /^\.flash\.rodata /  { fr=$2 }
    /^\.flash\.appdesc / { fa=$2 } /^\.dram0\.data /    { dd=$2 }
    /^\.dram0\.bss /     { db=$2 } /^\.iram0\.text /    { it=$2 }
    /^\.iram0\.vectors / { iv=$2 }
    END { printf "%d %d %d\n", ft+fr+fa+dd, dd+db, it+iv }
  '
)

# Per-owner attribution (App / Deps / SDK) for the next commit's deltas.
APP_F=0; APP_R=0; DEP_F=0; DEP_R=0; SDK_F=0; SDK_R=0
if [ -n "$NM_TOOL" ] && [ -x "$NM_TOOL" ]; then
  read -r APP_F APP_R DEP_F DEP_R SDK_F SDK_R < <(
    "$NM_TOOL" --radix=d --print-size --size-sort "$ELF" 2>/dev/null | awk -v app="$APP_CRATE" '
      NF>=4 {
        size = $2 + 0
        if (size > 524288) next
        typ = $3; name = $4; for (i=5;i<=NF;i++) name = name " " $i
        if      (index(name, app)) o = "app"
        else if (name ~ /^_(R|ZN)/) o = "dep"
        else                        o = "sdk"
        if      (typ ~ /^[tTrR]$/) flash[o] += size
        else if (typ ~ /^[dD]$/)   { flash[o] += size; ram[o] += size }
        else if (typ ~ /^[bB]$/)   ram[o]   += size
      }
      END { printf "%d %d %d %d %d %d\n",
        flash["app"]+0, ram["app"]+0, flash["dep"]+0, ram["dep"]+0, flash["sdk"]+0, ram["sdk"]+0 }'
  )
fi

# Read old baseline for delta (before overwriting it)
PREV_FLASH=0; PREV_DRAM=0; PREV_IRAM=0; PREV_PROFILE=""
if [ -f "$BASELINE" ]; then
  while IFS='=' read -r k v; do
    case "$k" in
      flash)   PREV_FLASH=$v   ;;
      dram)    PREV_DRAM=$v    ;;
      iram)    PREV_IRAM=$v    ;;
      profile) PREV_PROFILE=$v ;;
    esac
  done < "$BASELINE"
fi

kb()  { awk "BEGIN { printf \"%d\", ($1 + 512) / 1024 }"; }
sgn() {
  local d=$(( $1 ))
  if   [ "$d" -gt 0 ]; then printf "+%d" $d
  elif [ "$d" -lt 0 ]; then printf "%d"  $d
  else                       printf "0"
  fi
}

DATE=$(date "+%Y-%m-%d %H:%M")

# Subject = first non-blank, non-comment line of the commit message being made.
SUBJECT="(no message)"
if [ -n "$MSGFILE" ] && [ -f "$MSGFILE" ]; then
  SUBJECT=$(grep -v '^#' "$MSGFILE" | sed '/^[[:space:]]*$/d' | head -1)
  [ -z "$SUBJECT" ] && SUBJECT="(no message)"
fi

# Create log with header on first use
if [ ! -f "$LOG" ]; then
  cat > "$LOG" <<'HEADER'
# ESP32-S3 Memory Footprint Log

Cumulative flash and RAM cost per commit (debug build unless P=r).

- **Flash** = `.flash.text` + `.flash.rodata` + `.dram0.data` + `.flash.appdesc`
- **DRAM**  = `.dram0.data` + `.dram0.bss` (runtime RAM)
- **IRAM**  = `.iram0.text` + `.iram0.vectors` (fast RAM / ISR code)
- **P**: `d` = debug, `r` = release
- **Δ columns**: bytes vs previous commit's build (0 = no embedded change in this commit)

| Date             | P | Flash KB | ΔFlash B | DRAM KB | ΔDRAM B | IRAM KB | ΔIRAM B | Subject |
|------------------|---|----------|----------|---------|---------|---------|---------|---------|
HEADER
fi

# Append one row for the commit being created
printf "| %-16s | %-1s | %8d | %8s | %7d | %7s | %7d | %7s | %s |\n" \
  "$DATE" "${PROFILE:0:1}" \
  "$(kb $FLASH)" "$(sgn $(( FLASH - PREV_FLASH )))" \
  "$(kb $DRAM)"  "$(sgn $(( DRAM  - PREV_DRAM  )))" \
  "$(kb $IRAM)"  "$(sgn $(( IRAM  - PREV_IRAM  )))" \
  "$SUBJECT" >> "$LOG"

# Stage the row so it is part of THIS commit's tree snapshot. No amend.
git -C "$REPO_ROOT" add MEMORY_LOG.md 2>/dev/null || true

# Update the baseline for the next commit's deltas.
printf "flash=%d\ndram=%d\niram=%d\nprofile=%s\napp_flash=%d\napp_ram=%d\ndep_flash=%d\ndep_ram=%d\nsdk_flash=%d\nsdk_ram=%d\n" \
  "$FLASH" "$DRAM" "$IRAM" "$PROFILE" \
  "$APP_F" "$APP_R" "$DEP_F" "$DEP_R" "$SDK_F" "$SDK_R" > "$BASELINE"

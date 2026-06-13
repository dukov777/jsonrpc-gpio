#!/usr/bin/env bash
# Show ESP32-S3 flash/RAM footprint before a commit so the cost of each
# increment is visible. Called by the git pre-commit hook and the Claude Code
# PreToolUse(Bash(git commit *)) hook.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || realpath "$(dirname "$0")/../..")"
ELF_DEBUG="$REPO_ROOT/target/xtensa-esp32s3-espidf/debug/jsonrpc-gpio"
ELF_RELEASE="$REPO_ROOT/target/xtensa-esp32s3-espidf/release/jsonrpc-gpio"
BASELINE="$REPO_ROOT/.claude/esp32-size-baseline"
# Rust crate name of this app (symbols carry it as e.g. "_12jsonrpc_gpio").
# Anything else mangled (_R/_ZN) is a Rust dependency; the rest is the C SDK.
APP_CRATE="jsonrpc_gpio"

# Pick the most recently built ELF
ELF=""; PROFILE=""
if [ -f "$ELF_RELEASE" ] && { [ ! -f "$ELF_DEBUG" ] || [ "$ELF_RELEASE" -nt "$ELF_DEBUG" ]; }; then
  ELF="$ELF_RELEASE"; PROFILE="release"
elif [ -f "$ELF_DEBUG" ]; then
  ELF="$ELF_DEBUG"; PROFILE="debug"
else
  echo "esp32-size: no embedded build found — run 'cargo build-s3' first" >&2
  exit 0
fi

# Find the latest installed xtensa-esp32s3-elf-size
SIZE_TOOL=$(find ~/.espressif/tools/xtensa-esp-elf -name "xtensa-esp32s3-elf-size" 2>/dev/null | sort -V | tail -1)
if [ -z "$SIZE_TOOL" ] || [ ! -x "$SIZE_TOOL" ]; then
  echo "esp32-size: xtensa-esp32s3-elf-size not found — source ~/export-esp.sh" >&2
  exit 0
fi

# nm lets us attribute each symbol to App / Deps (Rust) / SDK (C). Optional —
# the section report below still prints if it is missing.
NM_TOOL=$(find ~/.espressif/tools/xtensa-esp-elf -name "xtensa-esp32s3-elf-nm" 2>/dev/null | sort -V | tail -1)

# Parse section sizes and compute flash/DRAM/IRAM totals
read -r FLASH DRAM IRAM FT FR DD DB IT < <(
  "$SIZE_TOOL" -A "$ELF" 2>/dev/null | awk '
    /^\.flash\.text /    { ft = $2 }
    /^\.flash\.rodata /  { fr = $2 }
    /^\.flash\.appdesc / { fa = $2 }
    /^\.dram0\.data /    { dd = $2 }
    /^\.dram0\.bss /     { db = $2 }
    /^\.iram0\.text /    { it = $2 }
    /^\.iram0\.vectors / { iv = $2 }
    END {
      # Flash = code + rodata + appdesc + initialized data (copied from flash at boot)
      # DRAM  = initialized data + BSS (runtime RAM)
      # IRAM  = fast RAM (ISR code + vectors)
      printf "%d %d %d %d %d %d %d %d\n",
             ft+fr+fa+dd, dd+db, it+iv, ft, fr, dd, db, it
    }
  '
)

# Attribute symbols to App / Deps / SDK. Each owner gets a Flash figure
# (code + rodata + the initializer image of .data) and a RAM figure
# (.data + .bss occupying SRAM at runtime) — mirroring the section model above.
APP_F=0; APP_R=0; DEP_F=0; DEP_R=0; SDK_F=0; SDK_R=0; HAVE_OWN=0
if [ -n "$NM_TOOL" ] && [ -x "$NM_TOOL" ]; then
  read -r APP_F APP_R DEP_F DEP_R SDK_F SDK_R < <(
    "$NM_TOOL" --radix=d --print-size --size-sort "$ELF" 2>/dev/null | awk -v app="$APP_CRATE" '
      NF>=4 {
        size = $2 + 0
        if (size > 524288) next   # skip linker region markers (> 512KB SRAM)
        typ = $3; name = $4; for (i=5;i<=NF;i++) name = name " " $i
        if      (index(name, app)) o = "app"
        else if (name ~ /^_(R|ZN)/) o = "dep"
        else                        o = "sdk"
        if      (typ ~ /^[tTrR]$/) flash[o] += size
        else if (typ ~ /^[dD]$/)   { flash[o] += size; ram[o] += size }
        else if (typ ~ /^[bB]$/)   ram[o]   += size
      }
      END {
        printf "%d %d %d %d %d %d\n",
          flash["app"]+0, ram["app"]+0, flash["dep"]+0, ram["dep"]+0, flash["sdk"]+0, ram["sdk"]+0
      }'
  )
  HAVE_OWN=1
fi

kb()  { awk "BEGIN { printf \"%d\", ($1 + 512) / 1024 }"; }
sgn() {
  local d=$(( $1 ))
  if   [ "$d" -gt 0 ]; then printf "+%d B" $d
  elif [ "$d" -lt 0 ]; then printf "%d B"  $d
  else                       printf "±0 B"
  fi
}

# Read previous baseline (written by esp32-size-record.sh after each commit)
PREV_FLASH=0; PREV_DRAM=0; PREV_IRAM=0; PREV_PROFILE=""
PREV_APP_F=""; PREV_APP_R=0; PREV_DEP_F=0; PREV_DEP_R=0; PREV_SDK_F=0; PREV_SDK_R=0
if [ -f "$BASELINE" ]; then
  while IFS='=' read -r k v; do
    case "$k" in
      flash)    PREV_FLASH=$v   ;;
      dram)     PREV_DRAM=$v    ;;
      iram)     PREV_IRAM=$v    ;;
      profile)  PREV_PROFILE=$v ;;
      app_flash) PREV_APP_F=$v  ;;
      app_ram)   PREV_APP_R=$v  ;;
      dep_flash) PREV_DEP_F=$v  ;;
      dep_ram)   PREV_DEP_R=$v  ;;
      sdk_flash) PREV_SDK_F=$v  ;;
      sdk_ram)   PREV_SDK_R=$v  ;;
    esac
  done < "$BASELINE"
fi

echo ""
printf "┌─ ESP32-S3 footprint (%s) ────────────────────────────────────────────\n" "$PROFILE"
printf "│  Flash  %5d KB   .flash.text %d KB  .flash.rodata %d KB  .data %d KB\n" \
  "$(kb $FLASH)" "$(kb $FT)" "$(kb $FR)" "$(kb $DD)"
printf "│  DRAM   %5d KB   .data %d KB  .bss %d KB\n" \
  "$(kb $DRAM)" "$(kb $DD)" "$(kb $DB)"
printf "│  IRAM   %5d KB   .iram0.text %d KB  .vectors 1 KB\n" \
  "$(kb $IRAM)" "$(kb $IT)"
if [ -n "$PREV_PROFILE" ]; then
  printf "│  Δ      Flash %-10s  DRAM %-10s  IRAM %-10s  (vs last commit, %s)\n" \
    "$(sgn $(( FLASH - PREV_FLASH )))" \
    "$(sgn $(( DRAM  - PREV_DRAM  )))" \
    "$(sgn $(( IRAM  - PREV_IRAM  )))" \
    "$PREV_PROFILE"
else
  printf "│  (no baseline yet — will be recorded after this commit)\n"
fi

# Ownership breakdown: App (your crate) / Deps (Rust deps + std) / SDK (ESP-IDF C).
# Prints a per-owner Flash/RAM delta vs the last commit when the baseline has it.
if [ "$HAVE_OWN" = "1" ]; then
  # Per-owner deltas only when the baseline carried the new keys.
  HAVE_OWN_PREV=0; [ -n "$PREV_APP_F" ] && HAVE_OWN_PREV=1
  own_row() { # name cur_flash cur_ram prev_flash prev_ram
    if [ "$HAVE_OWN_PREV" = "1" ]; then
      printf "│  %-5s  Flash %4d KB (%-10s)  RAM %4d KB (%-10s)\n" \
        "$1" "$(kb $2)" "$(sgn $(( $2 - $4 )))" "$(kb $3)" "$(sgn $(( $3 - $5 )))"
    else
      printf "│  %-5s  Flash %4d KB                RAM %4d KB\n" \
        "$1" "$(kb $2)" "$(kb $3)"
    fi
  }
  printf "├─ ownership (symbol-attributed) ───────────────────────────────────────\n"
  own_row "App"  "$APP_F" "$APP_R" "$PREV_APP_F" "$PREV_APP_R"
  own_row "Deps" "$DEP_F" "$DEP_R" "$PREV_DEP_F" "$PREV_DEP_R"
  own_row "SDK"  "$SDK_F" "$SDK_R" "$PREV_SDK_F" "$PREV_SDK_R"
fi
printf "└──────────────────────────────────────────────────────────────────────\n"
echo ""

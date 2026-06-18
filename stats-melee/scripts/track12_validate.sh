#!/bin/bash
# Track 12 pre-12f validations.
#
# Runs three short Dolphin tests back-to-back to lock in design decisions
# before rewriting render_worker. Each test uses a small DTM (~10s of game
# time) so the interpreter finishes in ~2 minutes per test.
#
# Logs everything to /tmp/track12_validations.log. Total runtime ~6-10 min.
#
# Note: macOS doesn't ship a `timeout` binary, so we use a Bash background-
# watchdog pattern instead.
#
# Prereqs (already done in previous session, verify if reusing):
# - Dolphin built at /Users/matthewlafrance/Dev/dolphin/Build/Binaries/Dolphin.app
# - That binary re-signed with /tmp/debug_entitlements.plist
# - ~/Library/Application Support/Dolphin/Config/Dolphin.ini has CPUCore=0,
#   SIDevice0=6, SIDevice1=6, [Movie] DumpFrames=True, [DSP] DumpAudio=True
# - GFX.ini has DumpFrames=True, DumpFramesAsImages=False

set -uo pipefail

DOLPHIN=/Users/matthewlafrance/Dev/dolphin/Build/Binaries/Dolphin.app/Contents/MacOS/Dolphin
ISO="/Users/matthewlafrance/Dev/slippi/Super Smash Bros. Melee (USA) (En,Ja) (v1.02).iso"
SLP=/Users/matthewlafrance/Dev/slippi/test_slps/Game_20250402T140144.slp
LOG=/tmp/track12_validations.log
DUMP_GLOBAL=~/Library/Application\ Support/Dolphin/Dump

: > "$LOG"
log() { echo "$@" | tee -a "$LOG"; }

cd /Users/matthewlafrance/Dev/slippi/stats-melee

log "=== Building DTMs ==="
cargo run --quiet --bin slp_to_dtm_bin -- --single-port --pad-prefix=1200 "$SLP" /tmp/val_p1.dtm >>"$LOG" 2>&1 \
  && log "  single-port DTM ready: /tmp/val_p1.dtm"
cargo run --quiet --bin slp_to_dtm_bin -- --pad-prefix=1200 "$SLP" /tmp/val_p2.dtm >>"$LOG" 2>&1 \
  && log "  multi-port DTM ready: /tmp/val_p2.dtm"

# Run command with a watchdog timeout. Sets WATCHDOG_TIMED_OUT=1 if the
# watchdog had to kill the child. Sets RUN_EXIT_CODE to the child's exit
# status (or its kill signal + 128 if killed).
run_with_timeout() {
  local timeout_s=$1; shift
  WATCHDOG_TIMED_OUT=0

  "$@" &
  local pid=$!

  (
    local i=0
    while [ $i -lt "$timeout_s" ]; do
      sleep 1
      kill -0 "$pid" 2>/dev/null || exit 0
      i=$((i+1))
    done
    # Timed out
    kill -TERM "$pid" 2>/dev/null
    sleep 2
    kill -KILL "$pid" 2>/dev/null
    exit 99
  ) &
  local wd_pid=$!

  wait "$pid" 2>/dev/null
  RUN_EXIT_CODE=$?

  # If watchdog is still alive, Dolphin finished on its own; clean up watchdog.
  # If watchdog already exited (it triggered the kill), set the timeout flag.
  if kill -0 "$wd_pid" 2>/dev/null; then
    kill "$wd_pid" 2>/dev/null
    wait "$wd_pid" 2>/dev/null
  else
    wait "$wd_pid" 2>/dev/null
    if [ $? -eq 99 ]; then
      WATCHDOG_TIMED_OUT=1
    fi
  fi
}

run_test() {
  local name=$1 dtm=$2 user_arg=$3 timeout_s=$4
  log ""
  log "=== $name ==="
  log "  DTM: $dtm"
  log "  user_arg: ${user_arg:-<default global user dir>}"
  log "  timeout: ${timeout_s}s"
  rm -rf "$DUMP_GLOBAL"/Frames "$DUMP_GLOBAL"/Audio
  local start=$(date +%s)

  if [ -n "$user_arg" ]; then
    run_with_timeout "$timeout_s" "$DOLPHIN" --user="$user_arg" --exec="$ISO" --movie="$dtm" --batch >>"$LOG" 2>&1
  else
    run_with_timeout "$timeout_s" "$DOLPHIN" --exec="$ISO" --movie="$dtm" --batch >>"$LOG" 2>&1
  fi

  local end=$(date +%s)
  local wall=$((end-start))
  log "  exit_code=$RUN_EXIT_CODE wall_seconds=$wall watchdog_killed=$WATCHDOG_TIMED_OUT"

  if [ "$WATCHDOG_TIMED_OUT" -eq 1 ]; then
    log "  RESULT: TIMED OUT after ${timeout_s}s — Dolphin did NOT auto-exit"
  elif [ $RUN_EXIT_CODE -eq 0 ]; then
    log "  RESULT: clean exit (--batch worked)"
  else
    log "  RESULT: non-zero exit ($RUN_EXIT_CODE) — likely a crash"
  fi

  # Wherever output landed (global or user-arg)
  local dump_root
  if [ -n "$user_arg" ]; then dump_root="$user_arg/Dump"; else dump_root="$DUMP_GLOBAL"; fi
  log "  Frames in $dump_root/Frames:"
  ls "$dump_root"/Frames 2>/dev/null | head -5 | sed 's/^/    /' | tee -a "$LOG"
  log "  Audio in $dump_root/Audio:"
  ls "$dump_root"/Audio 2>/dev/null | head -5 | sed 's/^/    /' | tee -a "$LOG"
}

# Test 1: --batch flag with the known-good single-port DTM. Want clean auto-exit.
run_test "Validation 1: --batch auto-exit (single-port)" /tmp/val_p1.dtm "" 600

# Test 2: same as Test 1 but with multi-port DTM. Want no crash.
run_test "Validation 2: multi-port DTM (P1+P2)" /tmp/val_p2.dtm "" 600

# Test 3: isolated user-dir. Pre-populate it with the known-good config.
USERDIR=/tmp/dolphin_user_test
rm -rf "$USERDIR"
mkdir -p "$USERDIR/Config"
cp ~/Library/Application\ Support/Dolphin/Config/Dolphin.ini "$USERDIR/Config/"
cp ~/Library/Application\ Support/Dolphin/Config/GFX.ini "$USERDIR/Config/"
run_test "Validation 3: --user=<tempdir>" /tmp/val_p1.dtm "$USERDIR" 600

log ""
log "=== ALL DONE ==="
log "Full log: $LOG"
log ""
log "Decisions to make based on these results:"
log "  - V1 clean exit → render_worker can rely on --batch"
log "  - V1 timed out → render_worker has to detect end-of-movie another way"
log "  - V2 clean → multi-port works, drop --single-port from default"
log "  - V2 crash → keep --single-port for now, deal with port-2 later"
log "  - V3 finds output in $USERDIR/Dump → use isolated user dirs in render_worker"
log "  - V3 finds output in global dir → --user not honored, need different isolation"

#!/usr/bin/env bash

set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)"

RUNTIME_DIR="${CODEX_METER_RUNTIME_DIR:-$REPO_ROOT/.runtime}"
PID_FILE="$RUNTIME_DIR/codex-meter.pid"
LOG_FILE="$RUNTIME_DIR/codex-meter.log"
DB_PATH="${CODEX_METER_DB:-$RUNTIME_DIR/codex-meter-seven.sqlite}"
CODEX_HOME_PATH="${CODEX_HOME:-${HOME}/.codex}"
PORT="${CODEX_METER_PORT:-18778}"
HEALTH_TIMEOUT_SECONDS="${CODEX_METER_HEALTH_TIMEOUT_SECONDS:-60}"
BIND_ADDRESS="127.0.0.1:$PORT"
BASE_URL="http://$BIND_ADDRESS"
BIN="${CODEX_METER_BIN:-$REPO_ROOT/target/debug/codex-meter}"

die() {
  printf 'codex-meter: %s\n' "$*" >&2
  exit 1
}

read_pid() {
  [ -f "$PID_FILE" ] || return 1
  pid="$(tr -d '[:space:]' < "$PID_FILE")"
  case "$pid" in
    ''|*[!0-9]*) return 1 ;;
    *) printf '%s\n' "$pid" ;;
  esac
}

is_codex_meter_pid() {
  pid="$1"
  command_line="$(ps -p "$pid" -o command= 2>/dev/null || true)"
  case "$command_line" in
    *codex-meter*) return 0 ;;
    *) return 1 ;;
  esac
}

is_healthy() {
  curl --silent --show-error --fail "$BASE_URL/api/health" >/dev/null 2>&1
}

ensure_binary() {
  if [ "${CODEX_METER_SKIP_BUILD:-0}" != "1" ]; then
    printf 'codex-meter: building %s\n' "$BIN"
    (cd "$REPO_ROOT" && cargo build --offline)
  fi
  [ -x "$BIN" ] || die "binary is not executable: $BIN"
}

wait_for_health() {
  case "$HEALTH_TIMEOUT_SECONDS" in
    ''|*[!0-9]*) die "CODEX_METER_HEALTH_TIMEOUT_SECONDS must be a non-negative integer" ;;
  esac
  attempts=0
  max_attempts=$((HEALTH_TIMEOUT_SECONDS * 2))
  while [ "$attempts" -lt "$max_attempts" ]; do
    if is_healthy; then
      return 0
    fi
    attempts=$((attempts + 1))
    sleep 0.5
  done
  return 1
}

stop_process() {
  pid="$(read_pid 2>/dev/null || true)"
  if [ -z "$pid" ]; then
    rm -f "$PID_FILE"
    return 0
  fi
  if ! kill -0 "$pid" 2>/dev/null; then
    rm -f "$PID_FILE"
    return 0
  fi
  if ! is_codex_meter_pid "$pid"; then
    die "refusing to stop unexpected process $pid"
  fi

  kill "$pid" 2>/dev/null || true
  attempts=0
  while kill -0 "$pid" 2>/dev/null && [ "$attempts" -lt 40 ]; do
    attempts=$((attempts + 1))
    sleep 0.25
  done
  if kill -0 "$pid" 2>/dev/null; then
    printf 'codex-meter: graceful stop timed out; forcing process %s\n' "$pid" >&2
    kill -KILL "$pid" 2>/dev/null || true
  fi
  rm -f "$PID_FILE"
}

start_service() {
  mkdir -p "$RUNTIME_DIR" "$(dirname -- "$DB_PATH")"

  pid="$(read_pid 2>/dev/null || true)"
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    if is_codex_meter_pid "$pid"; then
      if is_healthy; then
        printf 'codex-meter is already running (pid %s)\n' "$pid"
        printf 'Web: %s/\n' "$BASE_URL"
        return 0
      fi
      printf 'codex-meter process %s exists but is not healthy; restarting it\n' "$pid" >&2
      stop_process
    else
      die "PID file points to unexpected process $pid"
    fi
  else
    rm -f "$PID_FILE"
  fi

  ensure_binary
  printf 'codex-meter: starting on %s\n' "$BASE_URL"
  printf 'codex-meter: database %s\n' "$DB_PATH"
  printf 'codex-meter: log %s\n' "$LOG_FILE"

  : > "$LOG_FILE"
  (
    cd "$REPO_ROOT"
    nohup env \
      CODEX_METER_DB="$DB_PATH" \
      CODEX_HOME="$CODEX_HOME_PATH" \
      CODEX_METER_BIND="$BIND_ADDRESS" \
      "$BIN" >>"$LOG_FILE" 2>&1 < /dev/null &
    printf '%s\n' "$!" > "$PID_FILE"
  )

  if ! wait_for_health; then
    printf 'codex-meter: failed to become healthy; recent log:\n' >&2
    tail -n 60 "$LOG_FILE" >&2 || true
    stop_process || true
    return 1
  fi

  pid="$(read_pid)"
  printf 'codex-meter is running (pid %s)\n' "$pid"
  printf 'Web: %s/\n' "$BASE_URL"
}

status_service() {
  pid="$(read_pid 2>/dev/null || true)"
  if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then
    printf 'codex-meter is stopped\n'
    return 1
  fi
  if ! is_codex_meter_pid "$pid"; then
    printf 'codex-meter PID file points to unexpected process %s\n' "$pid" >&2
    return 1
  fi
  if is_healthy; then
    printf 'codex-meter is running (pid %s)\n' "$pid"
    printf 'Web: %s/\n' "$BASE_URL"
    printf 'Health: %s/api/health\n' "$BASE_URL"
    printf 'Log: %s\n' "$LOG_FILE"
    return 0
  fi
  printf 'codex-meter process %s is running but API is not healthy\n' "$pid" >&2
  return 1
}

usage() {
  cat <<'USAGE'
Usage: scripts/codex-meter-service.sh <command>

Commands:
  start    Build if necessary and start the API + Web service.
  stop     Stop the service started by this script.
  restart  Stop and start the service.
  status   Show process and health status.
  logs     Show the latest service log lines.

Environment overrides:
  CODEX_METER_PORT       HTTP port (default: 18778)
  CODEX_METER_HEALTH_TIMEOUT_SECONDS  Startup health wait (default: 60; increase for first large history scan)
  CODEX_METER_DB         SQLite path (default: .runtime/codex-meter-seven.sqlite)
  CODEX_HOME             Codex home containing sessions/ and archived_sessions/
  CODEX_METER_BIN        Backend binary path
  CODEX_METER_SKIP_BUILD  Set to 1 to reuse an already-built binary
USAGE
}

command="${1:-}"
case "$command" in
  start) start_service ;;
  stop) stop_process ;;
  restart) stop_process; start_service ;;
  status) status_service ;;
  logs)
    mkdir -p "$RUNTIME_DIR"
    touch "$LOG_FILE"
    tail -n 100 "$LOG_FILE"
    ;;
  *) usage; exit 2 ;;
esac

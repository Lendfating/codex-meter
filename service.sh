#!/usr/bin/env bash

set -eu

REPO_ROOT="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

RUNTIME_DIR="$REPO_ROOT/.runtime"
PID_FILE="$RUNTIME_DIR/codex-meter.pid"
LOG_FILE="$RUNTIME_DIR/codex-meter.log"
DB_PATH="$RUNTIME_DIR/codex-meter.sqlite"
BIN="$REPO_ROOT/target/debug/codex-meter"

# Command-line defaults; overridden by --options below.
PORT=18778
SKIP_BUILD=0
CCUSAGE_ON=0

die() {
  printf 'codex-meter: %s\n' "$*" >&2
  exit 1
}

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --port)
        [ "$#" -ge 2 ] || die "--port requires a value"
        case "$2" in
          ''|*[!0-9]*) die "--port must be a non-negative integer: $2" ;;
        esac
        PORT="$2"
        shift 2
        ;;
      --no-build)
        SKIP_BUILD=1
        shift
        ;;
      --ccusage)
        CCUSAGE_ON=1
        shift
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        die "unknown option: $1"
        ;;
    esac
  done
  BIND_ADDRESS="127.0.0.1:$PORT"
  BASE_URL="http://$BIND_ADDRESS"
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
  if [ "$SKIP_BUILD" != "1" ]; then
    printf 'codex-meter: building %s\n' "$BIN"
    (cd "$REPO_ROOT" && cargo build --offline)
  fi
  [ -x "$BIN" ] || die "binary is not executable: $BIN"
}

wait_for_health() {
  max_attempts=10
  attempts=0
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
  mkdir -p "$RUNTIME_DIR"

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
    start_env=(
      CODEX_METER_BIND="$BIND_ADDRESS"
    )
    [ "$CCUSAGE_ON" = "1" ] && start_env+=(CODEX_METER_CCUSAGE_ON=1)
    nohup env "${start_env[@]}" "$BIN" >>"$LOG_FILE" 2>&1 < /dev/null &
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

show_logs() {
  mkdir -p "$RUNTIME_DIR"
  touch "$LOG_FILE"
  tail -n 100 "$LOG_FILE"
}

open_web() {
  if is_healthy; then
    open "$BASE_URL"
  else
    printf 'codex-meter is not running; start it first:\n  ./service.sh start\n' >&2
    return 1
  fi
}

usage() {
  cat <<'USAGE'
Usage: ./service.sh <command> [options]

Commands:
  start    Build if necessary and start the API + Web service.
  stop     Stop the service started by this script.
  restart  Stop and start the service.
  status   Show process and health status.
  logs     Show the latest service log lines.
  web      Open the dashboard in a browser.

Options (start/restart):
  --port N                 HTTP port (default: 18778)
  --no-build               Reuse an already-built binary instead of running cargo build
  --ccusage                Enable ccusage reconciliation (runs on boot, then hourly; also on POST /api/refresh)

Examples:
  ./service.sh start
  ./service.sh start --port 18779
  ./service.sh start --no-build
  ./service.sh restart --ccusage
USAGE
}

command="${1:-}"
[ "$#" -ge 1 ] && shift
case "$command" in
  start) parse_args "$@"; start_service ;;
  stop) parse_args "$@"; stop_process ;;
  restart) parse_args "$@"; stop_process; start_service ;;
  status) parse_args "$@"; status_service ;;
  logs) parse_args "$@"; show_logs ;;
  web) parse_args "$@"; open_web ;;
  -h|--help) usage; exit 0 ;;
  "") usage; exit 2 ;;
  *) die "unknown command: $command" ;;
esac

#!/usr/bin/env bash

set -eu

REPO_ROOT="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"

RUNTIME_DIR="$REPO_ROOT/.runtime"
PID_FILE="$RUNTIME_DIR/codex-meter.pid"
LOG_FILE="$RUNTIME_DIR/codex-meter.log"
DB_PATH="$RUNTIME_DIR/codex-meter.sqlite"
BIN="$REPO_ROOT/target/release/codex-meter"

# Command-line defaults; overridden by --options below.
PORT=18778
SKIP_BUILD=0
CCUSAGE_ON=1
FROM_DATE=""

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
      --from)
        [ "$#" -ge 2 ] || die "--from requires a value in YYYY-MM-DD format"
        case "$2" in
          ????-??-??) FROM_DATE="$2" ;;
          *) die "--from must use YYYY-MM-DD format: $2" ;;
        esac
        shift 2
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
    *"$BIN"*) return 0 ;;
    *) return 1 ;;
  esac
}

is_healthy() {
  curl --silent --show-error --fail "$BASE_URL/api/health" >/dev/null 2>&1
}

health_payload() {
  curl --silent --show-error --fail "$BASE_URL/api/health"
}

is_ready() {
  health_payload 2>/dev/null | tr -d '[:space:]' | grep -q '"data_ready":true'
}

is_sync_failed() {
  health_payload 2>/dev/null | tr -d '[:space:]' | grep -q '"sync_phase":"failed"'
}

sync_phase() {
  health_payload 2>/dev/null | sed -n 's/.*"sync_phase":"\([^"]*\)".*/\1/p'
}

ensure_binary() {
  if [ "$SKIP_BUILD" != "1" ]; then
    printf 'codex-meter: building %s\n' "$BIN"
    (cd "$REPO_ROOT" && cargo build --release --offline)
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

wait_for_process() {
  expected_pid="$1"
  max_attempts=10
  attempts=0
  while [ "$attempts" -lt "$max_attempts" ]; do
    if kill -0 "$expected_pid" 2>/dev/null && is_codex_meter_pid "$expected_pid"; then
      return 0
    fi
    attempts=$((attempts + 1))
    sleep 0.5
  done
  return 1
}

wait_for_ready() {
  max_attempts=150
  attempts=0
  while [ "$attempts" -lt "$max_attempts" ]; do
    if is_ready; then
      return 0
    fi
    if is_sync_failed; then
      return 2
    fi
    attempts=$((attempts + 1))
    sleep 2
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
        if is_ready; then
          printf 'codex-meter is already running (pid %s)\n' "$pid"
          printf 'Data: ready\n'
          printf 'Web: %s/\n' "$BASE_URL"
          return 0
        fi
        printf 'codex-meter is already running (pid %s); waiting for data readiness\n' "$pid"
        ready_result=0
        wait_for_ready || ready_result=$?
        if [ "$ready_result" -eq 0 ]; then
          printf 'Data: ready\n'
          printf 'Web: %s/\n' "$BASE_URL"
          return 0
        fi
        if [ "$ready_result" -eq 2 ]; then
          printf 'codex-meter: initial data sync failed; recent log:\n' >&2
        else
          printf 'codex-meter: initial data sync did not finish within 5 minutes; recent log:\n' >&2
        fi
        tail -n 60 "$LOG_FILE" >&2 || true
        return 1
      fi
      printf 'codex-meter process %s exists but is not healthy; restarting it\n' "$pid" >&2
      stop_process
    else
      die "PID file points to unexpected process $pid"
    fi
  else
    rm -f "$PID_FILE"
  fi

  if is_healthy; then
    die "port $PORT is already served by an untracked codex-meter process; stop it before starting"
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
    [ -n "$FROM_DATE" ] && start_env+=(CODEX_METER_JSONL_FROM="$FROM_DATE")
    nohup env "${start_env[@]}" "$BIN" >>"$LOG_FILE" 2>&1 < /dev/null &
    printf '%s\n' "$!" > "$PID_FILE"
  )

  started_pid="$(read_pid 2>/dev/null || true)"
  if [ -z "$started_pid" ] || ! wait_for_process "$started_pid"; then
    printf 'codex-meter: newly started process did not stay alive; recent log:\n' >&2
    tail -n 60 "$LOG_FILE" >&2 || true
    stop_process || true
    return 1
  fi

  if ! wait_for_health; then
    printf 'codex-meter: failed to become healthy; recent log:\n' >&2
    tail -n 60 "$LOG_FILE" >&2 || true
    stop_process || true
    return 1
  fi

  ready_result=0
  if wait_for_ready; then
    :
  else
    ready_result=$?
    if [ "$ready_result" -eq 2 ]; then
      printf 'codex-meter: initial data sync failed; recent log:\n' >&2
    else
      printf 'codex-meter: initial data sync did not finish within 5 minutes; recent log:\n' >&2
    fi
    tail -n 60 "$LOG_FILE" >&2 || true
    stop_process || true
    return 1
  fi

  pid="$(read_pid)"
  printf 'codex-meter is running (pid %s)\n' "$pid"
  printf 'Data: ready\n'
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
    if is_ready; then
      printf 'Data: ready\n'
    else
      phase="$(sync_phase)"
      printf 'Data: loading%s\n' "${phase:+ ($phase)}"
    fi
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
  --ccusage                Keep ccusage reconciliation enabled (default; runs on boot, then hourly; also on POST /api/refresh)
  --from YYYY-MM-DD        Inclusive JSONL backfill start date (default: the last 30 calendar days)

Examples:
  ./service.sh start
  ./service.sh start --port 18779
  ./service.sh start --no-build
  ./service.sh restart --ccusage
  ./service.sh start --from 2026-01-01
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

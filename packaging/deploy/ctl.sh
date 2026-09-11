#!/usr/bin/env bash
# 进程管理脚本：随安装包 deploy/ 分发并复制到 <安装根>/deploy/ctl.sh。
#
# 后端模式由安装时写入的 <安装根>/deploy/mode 决定：
#   - systemd：安装时使用 --with-systemd，ctl.sh 调 systemctl 管理 unit
#   - direct ：安装时使用 --no-systemd，ctl.sh 用 PID 文件直接管理进程（无 systemd 环境）
#
# 用法：
#   ctl.sh <apiserver|gse-server|gse-agent|console> <start|stop|status|restart>
set -euo pipefail

USAGE="usage: ctl.sh <apiserver|gse-server|gse-agent|console> <start|stop|status|restart>"

COMPONENT="${1:-}"
ACTION="${2:-}"

case "$COMPONENT" in
  apiserver|gse-server|gse-agent|console) ;;
  dpc) echo "dpc is a one-shot CLI tool" >&2; exit 1 ;;
  *) echo "$USAGE" >&2; exit 1 ;;
esac

case "$ACTION" in
  start|stop|status|restart) ;;
  *) echo "$USAGE" >&2; exit 1 ;;
esac

DEPLOY_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_ROOT="$(cd "$DEPLOY_DIR/.." && pwd)"
MODE="systemd"
[[ -f "$DEPLOY_DIR/mode" ]] && MODE="$(cat "$DEPLOY_DIR/mode")"

direct_bin() { echo "$INSTALL_ROOT/$COMPONENT/bin/$COMPONENT"; }
direct_cwd() { echo "$INSTALL_ROOT/$COMPONENT"; }
direct_env() {
  case "$COMPONENT" in
    gse-server) echo "GSE_SERVER_CONFIG=$INSTALL_ROOT/gse-server/conf/gse-server.toml" ;;
    gse-agent)  echo "GSE_AGENT_CONFIG=$INSTALL_ROOT/gse-agent/conf/gse-agent.toml" ;;
    console)    echo "CONSOLE_CONFIG=$INSTALL_ROOT/console/conf/console.toml" ;;
    *)          echo "" ;;
  esac
}

PID_DIR="$INSTALL_ROOT/run"
LOG_DIR="$INSTALL_ROOT/logs"
PID_FILE="$PID_DIR/$COMPONENT.pid"
LOG_FILE="$LOG_DIR/$COMPONENT.log"

is_running() {
  [[ -f "$PID_FILE" ]] || return 1
  local pid
  pid="$(cat "$PID_FILE" 2>/dev/null || true)"
  [[ -n "$pid" ]] || return 1
  kill -0 "$pid" 2>/dev/null
}

direct_start() {
  if is_running; then
    echo "$COMPONENT already running (pid $(cat "$PID_FILE"))"
    return 0
  fi
  mkdir -p "$PID_DIR" "$LOG_DIR"
  local bin envkv
  bin="$(direct_bin)"
  envkv="$(direct_env)"
  if [[ ! -x "$bin" ]]; then
    echo "$COMPONENT binary not found or not executable: $bin" >&2
    exit 1
  fi
  (
    cd "$(direct_cwd)"
    [[ -n "$envkv" ]] && export $envkv
    if command -v setsid >/dev/null 2>&1; then
      setsid bash -c 'echo $$ > "$1"; shift; exec "$@"' _ "$PID_FILE" "$bin" </dev/null >>"$LOG_FILE" 2>&1 &
    else
      bash -c 'echo $$ > "$1"; shift; exec "$@"' _ "$PID_FILE" "$bin" </dev/null >>"$LOG_FILE" 2>&1 &
    fi
  )
  sleep 1
  if is_running; then
    echo "$COMPONENT started (pid $(cat "$PID_FILE"))"
    echo "log: $LOG_FILE"
  else
    echo "$COMPONENT failed to start, see $LOG_FILE" >&2
    exit 1
  fi
}

direct_stop() {
  if ! is_running; then
    if [[ -f "$PID_FILE" ]]; then
      rm -f "$PID_FILE"
    fi
    echo "$COMPONENT not running"
    return 0
  fi
  local pid
  pid="$(cat "$PID_FILE")"
  kill "$pid" 2>/dev/null || true
  local i
  for i in $(seq 1 20); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.5
  done
  if kill -0 "$pid" 2>/dev/null; then
    echo "$COMPONENT did not exit after SIGTERM, sending SIGKILL" >&2
    kill -9 "$pid" 2>/dev/null || true
  fi
  rm -f "$PID_FILE"
  echo "$COMPONENT stopped"
}

direct_status() {
  if is_running; then
    echo "$COMPONENT running (pid $(cat "$PID_FILE"))"
    return 0
  fi
  echo "$COMPONENT stopped"
  return 3
}

if [[ "$MODE" == "direct" ]]; then
  case "$ACTION" in
    start)   direct_start ;;
    stop)    direct_stop ;;
    status)  direct_status ;;
    restart) direct_stop; direct_start ;;
  esac
  exit $?
fi

if ! command -v systemctl >/dev/null 2>&1 || [[ ! -d /run/systemd/system ]]; then
  echo "systemd required: install with --no-systemd for PID-file mode" >&2
  exit 1
fi

UNIT="vectorman-${COMPONENT}.service"
exec systemctl "$ACTION" "$UNIT"

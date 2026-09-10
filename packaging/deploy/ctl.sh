#!/usr/bin/env bash
# 进程管理脚本：systemd 封装，随安装包 deploy/ 分发并复制到 <安装根>/deploy/ctl.sh。
#
# 用法：
#   ctl.sh <apiserver|gse-server|gse-agent> <start|stop|status|restart>
set -euo pipefail

USAGE="usage: ctl.sh <apiserver|gse-server|gse-agent> <start|stop|status|restart>"

COMPONENT="${1:-}"
ACTION="${2:-}"

case "$COMPONENT" in
  apiserver|gse-server|gse-agent) ;;
  dpc) echo "dpc is a one-shot CLI tool" >&2; exit 1 ;;
  *) echo "$USAGE" >&2; exit 1 ;;
esac

case "$ACTION" in
  start|stop|status|restart) ;;
  *) echo "$USAGE" >&2; exit 1 ;;
esac

if ! command -v systemctl >/dev/null 2>&1 || [[ ! -d /run/systemd/system ]]; then
  echo "systemd required: ctl.sh only manages services via systemd" >&2
  exit 1
fi

UNIT="vectorman-${COMPONENT}.service"
exec systemctl "$ACTION" "$UNIT"

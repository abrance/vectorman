#!/usr/bin/env bash
# 目标机安装脚本：随安装包 deploy/ 分发。
#
# 用法：
#   install.sh <apiserver|dpc|gse-server|gse-agent|all> [--dest /opt/vectorman] [--with-systemd]
set -euo pipefail

USAGE="usage: install.sh <apiserver|dpc|gse-server|gse-agent|all> [--dest /opt/vectorman] [--with-systemd]"

COMPONENT_ARG=""
DEST="/opt/vectorman"
WITH_SYSTEMD=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dest) DEST="$2"; shift 2 ;;
    --with-systemd) WITH_SYSTEMD=1; shift ;;
    -h|--help) echo "$USAGE"; exit 0 ;;
    *) COMPONENT_ARG="$1"; shift ;;
  esac
done

case "$COMPONENT_ARG" in
  apiserver|dpc|gse-server|gse-agent) COMPONENTS=("$COMPONENT_ARG") ;;
  all) COMPONENTS=(apiserver dpc gse-server gse-agent) ;;
  *) echo "$USAGE" >&2; exit 1 ;;
esac

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PKG_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if [[ ! -d "$PKG_ROOT/gse-server/bin" ]]; then
  echo "not a package tree: $PKG_ROOT" >&2
  exit 1
fi

if ! mkdir -p "$DEST" 2>/dev/null; then
  echo "root required: cannot create $DEST" >&2
  exit 1
fi

mkdir -p "$DEST/deploy"
cp "$SCRIPT_DIR/ctl.sh" "$DEST/deploy/ctl.sh"
chmod +x "$DEST/deploy/ctl.sh"

DAEMON_RELOAD_NEEDED=0
for c in "${COMPONENTS[@]}"; do
  SRC="$PKG_ROOT/$c"
  DEST_C="$DEST/$c"
  mkdir -p "$DEST_C"

  cp -a "$SRC/bin" "$DEST_C/bin"
  cp -a "$SRC/conf" "$DEST_C/conf"
  if [[ "$c" == "gse-server" && -d "$SRC/web" ]]; then
    cp -a "$SRC/web" "$DEST_C/web"
  fi

  case "$c" in
    apiserver)    INSTANCE="$DEST_C/config.toml";       EXAMPLE="$DEST_C/conf/config.toml.example" ;;
    gse-server)   INSTANCE="$DEST_C/conf/gse-server.toml"; EXAMPLE="$DEST_C/conf/gse-server.toml.example" ;;
    gse-agent)    INSTANCE="$DEST_C/conf/gse-agent.toml";  EXAMPLE="$DEST_C/conf/gse-agent.toml.example" ;;
    dpc)          INSTANCE="" ;;
  esac

  if [[ -n "$INSTANCE" ]]; then
    if [[ -e "$INSTANCE" ]]; then
      echo "[$c] config kept: $INSTANCE"
    else
      cp "$EXAMPLE" "$INSTANCE"
      if [[ "$c" == "gse-server" && -d "$DEST_C/web" ]]; then
        printf '\n# enable single-port web hosting (web/ shipped in package)\nhttp_web_dir = "web"\n' >> "$INSTANCE"
      fi
      echo "[$c] config generated: $INSTANCE"
    fi
  fi

  if [[ "$WITH_SYSTEMD" -eq 1 ]]; then
    if [[ "$c" == "dpc" ]]; then
      echo "[dpc] one-shot CLI tool, no unit installed"
    else
      if [[ "$(id -u)" -ne 0 ]]; then
        echo "root required: --with-systemd writes /etc/systemd/system" >&2
        exit 1
      fi
      sed "s|@INSTALL_ROOT@|$DEST_C|g" "$SCRIPT_DIR/units/vectorman-$c.service.in" \
        > "/etc/systemd/system/vectorman-$c.service"
      DAEMON_RELOAD_NEEDED=1
      echo "[$c] unit installed: /etc/systemd/system/vectorman-$c.service"
    fi
  fi

  echo "[$c] installed: $DEST_C"
  if [[ "$c" != "dpc" ]]; then
    echo "[$c] start:  $DEST/deploy/ctl.sh $c start"
    echo "[$c] status: $DEST/deploy/ctl.sh $c status"
  fi
done

if [[ "$DAEMON_RELOAD_NEEDED" -eq 1 ]]; then
  if ! systemctl daemon-reload; then
    echo "daemon-reload failed; run 'systemctl daemon-reload' manually" >&2
  fi
fi

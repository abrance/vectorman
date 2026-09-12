#!/usr/bin/env bash
# 目标机安装脚本：随安装包 deploy/ 分发。
#
# 用法：
#   install.sh <apiserver|dpc|gse-server|gse-agent|vmctl|all> [--dest /opt/vectorman] [--with-systemd|--no-systemd]
set -euo pipefail

USAGE="usage: install.sh <apiserver|dpc|gse-server|gse-agent|vmctl|all> [--dest /opt/vectorman] [--with-systemd|--no-systemd]"

COMPONENT_ARG=""
DEST="/opt/vectorman"
WITH_SYSTEMD=0
NO_SYSTEMD=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dest) DEST="$2"; shift 2 ;;
    --with-systemd) WITH_SYSTEMD=1; shift ;;
    --no-systemd) NO_SYSTEMD=1; shift ;;
    -h|--help) echo "$USAGE"; exit 0 ;;
    *) COMPONENT_ARG="$1"; shift ;;
  esac
done

if [[ "$WITH_SYSTEMD" -eq 1 && "$NO_SYSTEMD" -eq 1 ]]; then
  echo "conflicting flags: --with-systemd and --no-systemd" >&2
  exit 1
fi

if [[ "$NO_SYSTEMD" -eq 1 ]]; then
  MODE="direct"
else
  MODE="systemd"
fi

case "$COMPONENT_ARG" in
  apiserver|dpc|gse-server|gse-agent|vmctl) COMPONENTS=("$COMPONENT_ARG") ;;
  all) COMPONENTS=(apiserver dpc gse-server gse-agent vmctl) ;;
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
if [[ "$(readlink -f "$SCRIPT_DIR/ctl.sh")" != "$(readlink -f "$DEST/deploy/ctl.sh")" ]]; then
  cp "$SCRIPT_DIR/ctl.sh" "$DEST/deploy/ctl.sh"
  chmod +x "$DEST/deploy/ctl.sh"
fi
printf '%s\n' "$MODE" > "$DEST/deploy/mode"
echo "process manager mode: $MODE"

DAEMON_RELOAD_NEEDED=0
IN_PLACE=0
if [[ "$(readlink -f "$PKG_ROOT")" == "$(readlink -f "$DEST")" ]]; then
  IN_PLACE=1
  echo "in-place reinstall: $PKG_ROOT (skipping bin/conf/web copy)"
fi
for c in "${COMPONENTS[@]}"; do
  SRC="$PKG_ROOT/$c"
  DEST_C="$DEST/$c"
  mkdir -p "$DEST_C"

  if [[ "$IN_PLACE" -eq 0 ]]; then
    cp -a "$SRC/bin" "$DEST_C/bin"
    cp -a "$SRC/conf" "$DEST_C/conf"
    if [[ "$c" == "gse-server" && -d "$SRC/web" ]]; then
      cp -a "$SRC/web" "$DEST_C/web"
    fi
  fi

  case "$c" in
    apiserver)    INSTANCE="$DEST_C/config.toml";       EXAMPLE="$DEST_C/conf/config.toml.example" ;;
    gse-server)   INSTANCE="$DEST_C/conf/gse-server.toml"; EXAMPLE="$DEST_C/conf/gse-server.toml.example" ;;
    gse-agent)    INSTANCE="$DEST_C/conf/gse-agent.toml";  EXAMPLE="$DEST_C/conf/gse-agent.toml.example" ;;
    dpc)          INSTANCE="" ;;
    vmctl)        INSTANCE="" ;;
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
    if [[ "$c" == "dpc" || "$c" == "vmctl" ]]; then
      echo "[$c] one-shot CLI tool, no unit installed"
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
  if [[ "$c" != "dpc" && "$c" != "vmctl" ]]; then
    echo "[$c] start:  $DEST/deploy/ctl.sh $c start"
    echo "[$c] status: $DEST/deploy/ctl.sh $c status"
  fi
done

if [[ "$DAEMON_RELOAD_NEEDED" -eq 1 ]]; then
  if ! systemctl daemon-reload; then
    echo "daemon-reload failed; run 'systemctl daemon-reload' manually" >&2
  fi
fi

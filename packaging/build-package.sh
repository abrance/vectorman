#!/usr/bin/env bash
# vectorman 统一打包入口：本地与 CI 共用。
# 产出 vectorman-<REL>-linux-x86_64.tar.gz，布局见 .monkeycode/specs/deploy-packaging/design.md。
#
# 用法：
#   packaging/build-package.sh [--version <v>]
#     [--bin-dir <dir>]   测试钩子：跳过 cargo build，直接使用现成二进制目录
#     [--dist-dir <dir>]  测试钩子：跳过前端构建，直接使用现成 dist 目录
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

VERSION=""
BIN_DIR=""
DIST_DIR=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --bin-dir) BIN_DIR="$2"; shift 2 ;;
    --dist-dir) DIST_DIR="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 1 ;;
  esac
done

if [[ -z "$VERSION" ]]; then
  VERSION="$(git describe --tags --always 2>/dev/null)" || {
    echo "git describe failed; pass --version explicitly" >&2
    exit 1
  }
fi
REL="${VERSION#v}"
ROOT="vectorman-${REL}-linux-x86_64"
TARBALL="${ROOT}.tar.gz"

step() { echo "==> $1"; }
fail() { echo "step $1 failed" >&2; exit 1; }

COMPONENTS=(apiserver dpc gse-server gse-agent)

if [[ -n "$BIN_DIR" ]]; then
  step "cargo-build skipped (bin-dir=$BIN_DIR)"
  if [[ ! -x "$BIN_DIR/gse-server" || ! -x "$BIN_DIR/gse-agent" || ! -x "$BIN_DIR/apiserver" || ! -x "$BIN_DIR/dpc" ]]; then
    echo "bin-dir missing one of: apiserver dpc gse-server gse-agent" >&2
    exit 1
  fi
else
  step "cargo-build"
  cargo build --release --workspace || fail cargo-build
  BIN_DIR="$REPO_ROOT/target/release"
fi

if [[ -n "$DIST_DIR" ]]; then
  step "frontend-build skipped (dist-dir=$DIST_DIR)"
  if [[ ! -f "$DIST_DIR/index.html" ]]; then
    echo "dist-dir missing index.html" >&2
    exit 1
  fi
else
  step "frontend-build"
  (
    cd frontend
    npm ci --no-audit --no-fund || fail frontend-build
    npm run build:node || fail frontend-build
  )
  DIST_DIR="$REPO_ROOT/frontend/apps/node/dist"
fi

step "assemble $ROOT"
rm -rf "$ROOT" "$TARBALL"
for c in "${COMPONENTS[@]}"; do
  mkdir -p "$ROOT/$c/bin" "$ROOT/$c/conf"
  cp "$BIN_DIR/$c" "$ROOT/$c/bin/"
done
cp config.toml.example "$ROOT/apiserver/conf/config.toml.example"
cp bins/gse-server/gse-server.toml.example "$ROOT/gse-server/conf/gse-server.toml.example"
cp bins/gse-agent/gse-agent.toml.example "$ROOT/gse-agent/conf/gse-agent.toml.example"
mkdir -p "$ROOT/gse-server/web"
cp -a "$DIST_DIR/." "$ROOT/gse-server/web/"
mkdir -p "$ROOT/deploy"
cp packaging/deploy/install.sh packaging/deploy/ctl.sh "$ROOT/deploy/"
cp -a packaging/deploy/units "$ROOT/deploy/units"
cp README.md "$ROOT/README.md"

if [[ ! -f "$ROOT/gse-server/web/index.html" ]]; then
  echo "web dist invalid: index.html missing" >&2
  exit 1
fi

step "strip"
strip "$ROOT"/*/bin/* 2>/dev/null || true

step "tar $TARBALL"
tar --sort=name --owner=0 --group=0 --numeric-owner \
    --mtime='UTC 1970-01-01' -czf "$TARBALL" "$ROOT"

ABS_TARBALL="$(cd "$REPO_ROOT" && pwd)/$TARBALL"
echo "package: $ABS_TARBALL ($(du -h "$TARBALL" | cut -f1))"

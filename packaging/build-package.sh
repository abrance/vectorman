#!/usr/bin/env bash
# vectorman 统一打包入口：本地与 CI 共用。
# 产出 vectorman-<REL>-linux-x86_64.tar.gz，布局见 .monkeycode/specs/deploy-packaging/design.md。
#
# 用法：
#   packaging/build-package.sh [--version <v>]
#     [--bin-dir <dir>]   测试钩子：跳过 cargo build，直接使用现成二进制目录
#     [--dist-dir <dir>]  测试钩子：跳过前端构建，直接使用现成 dist 目录
#     [--dataplane-dist-dir <dir>]  测试钩子：跳过 dataserver 前端构建，直接使用现成 dist 目录
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

VERSION=""
BIN_DIR=""
DIST_DIR=""
DATAPLANE_DIST_DIR=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="$2"; shift 2 ;;
    --bin-dir) BIN_DIR="$2"; shift 2 ;;
    --dist-dir) DIST_DIR="$2"; shift 2 ;;
    --dataplane-dist-dir) DATAPLANE_DIST_DIR="$2"; shift 2 ;;
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

COMPONENTS=(dataserver dpc gse-server gse-agent vmctl console)
BUILT_MUSL=0

if [[ -n "$BIN_DIR" ]]; then
  step "cargo-build skipped (bin-dir=$BIN_DIR)"
  if [[ ! -x "$BIN_DIR/gse-server" || ! -x "$BIN_DIR/gse-agent" || ! -x "$BIN_DIR/dataserver" || ! -x "$BIN_DIR/dpc" || ! -x "$BIN_DIR/vmctl" || ! -x "$BIN_DIR/console" ]]; then
    echo "bin-dir missing one of: dataserver dpc gse-server gse-agent vmctl console" >&2
    exit 1
  fi
else
  # eBPF 内核态对象文件：**发布期构建**而不是入库到 git（二进制入库会陈旧、评审噪音大，
  # 而 `gse-agent-ebpf/build.rs` 已经能「有就内嵌、没有就给出明确报错」）。
  #
  # bpf-linker 需要 LLVM：0.11 起要 LLVM 21+（Ubuntu 24.04 自带的 18 不够，需 apt.llvm.org）。
  # 没装 bpf-linker 时不报错，只是发布包里没有内嵌对象文件、采集项运行时会明确提示先构建。
  if command -v bpf-linker >/dev/null 2>&1; then
    step "ebpf-objects"
    "$REPO_ROOT/scripts/build-ebpf.sh" || fail ebpf-objects
  else
    step "ebpf-objects skipped (bpf-linker not found)"
    echo "提示：安装 bpf-linker（需 LLVM 21+）后重跑，发布包才会内嵌 packaging/ebpf/*.o" >&2
  fi

  step "cargo-build"
  MUSL_TARGET="x86_64-unknown-linux-musl"
  if ! command -v musl-gcc >/dev/null 2>&1; then
    echo "musl-gcc not found; install musl-tools" >&2
    fail cargo-build
  fi
  if command -v rustup >/dev/null 2>&1; then
    rustup target add "$MUSL_TARGET" || fail cargo-build
  fi
  export CC_x86_64_unknown_linux_musl="${CC_x86_64_unknown_linux_musl:-musl-gcc}"
  # rustc musl target 默认用 rust-lld 做静态链接。把 LINKER 设成 musl-gcc
  # 时，ring/ureq（vmctl、dpc）启动会 SIGSEGV。C 依赖只需 CC=musl-gcc。
  unset CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER
  # 注入发布版本到 --version；本地 cargo 未设该变量时回退 git describe。
  export VECTORMAN_VERSION="$REL"
  cargo build --release --workspace --target "$MUSL_TARGET" || fail cargo-build
  BIN_DIR="$REPO_ROOT/target/$MUSL_TARGET/release"
  BUILT_MUSL=1
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
    npm run build:console || fail frontend-build
    npm run build:dataplane || fail frontend-build
    npm run build:desktop || fail frontend-build
  )
  DIST_DIR="$REPO_ROOT/frontend/apps/console/dist"
fi

if [[ -n "$DATAPLANE_DIST_DIR" ]]; then
  step "dataplane-frontend-build skipped (dataplane-dist-dir=$DATAPLANE_DIST_DIR)"
else
  DATAPLANE_DIST_DIR="$REPO_ROOT/frontend/apps/dataplane/dist"
fi
if [[ ! -f "$DATAPLANE_DIST_DIR/index.html" ]]; then
  echo "dataplane dist missing index.html: $DATAPLANE_DIST_DIR" >&2
  exit 1
fi

step "assemble $ROOT"
rm -rf "$ROOT" "$TARBALL"
for c in "${COMPONENTS[@]}"; do
  mkdir -p "$ROOT/$c/bin" "$ROOT/$c/conf"
  cp "$BIN_DIR/$c" "$ROOT/$c/bin/"
done
cp config.toml.example "$ROOT/dataserver/conf/config.toml.example"
cp bins/gse-server/gse-server.toml.example "$ROOT/gse-server/conf/gse-server.toml.example"
cp bins/gse-agent/gse-agent.toml.example "$ROOT/gse-agent/conf/gse-agent.toml.example"
cp bins/console/console.toml.example "$ROOT/console/conf/console.toml.example"
mkdir -p "$ROOT/gse-server/web"
cp -a "$DIST_DIR/." "$ROOT/gse-server/web/"
mkdir -p "$ROOT/dataserver/web"
cp -a "$DATAPLANE_DIST_DIR/." "$ROOT/dataserver/web/"
DESKTOP_DIST="${DESKTOP_DIST:-$REPO_ROOT/frontend/apps/desktop/dist}"
if [[ ! -f "$DESKTOP_DIST/index.html" ]]; then
  echo "desktop dist missing index.html: $DESKTOP_DIST" >&2
  exit 1
fi
mkdir -p "$ROOT/console/web"
cp -a "$DESKTOP_DIST/." "$ROOT/console/web/"
mkdir -p "$ROOT/deploy"
cp packaging/deploy/install.sh packaging/deploy/ctl.sh "$ROOT/deploy/"
cp -a packaging/deploy/units "$ROOT/deploy/units"
cp -a packaging/deploy/k8s "$ROOT/deploy/k8s"
cp README.md "$ROOT/README.md"

if [[ ! -f "$ROOT/gse-server/web/index.html" ]]; then
  echo "web dist invalid: index.html missing" >&2
  exit 1
fi
if [[ ! -f "$ROOT/console/web/index.html" ]]; then
  echo "console web dist invalid: index.html missing" >&2
  exit 1
fi
if [[ ! -f "$ROOT/dataserver/web/index.html" ]]; then
  echo "dataserver web dist invalid: index.html missing" >&2
  exit 1
fi

step "strip"
strip "$ROOT"/*/bin/* 2>/dev/null || true

if [[ "$BUILT_MUSL" -eq 1 ]]; then
  step "verify-static"
  for c in "${COMPONENTS[@]}"; do
    info="$(ldd "$ROOT/$c/bin/$c" 2>&1 || true)"
    echo "$c: $info"
    if grep -Eq 'libc\.so|ld-linux' <<<"$info"; then
      echo "expected musl static binary without glibc: $ROOT/$c/bin/$c" >&2
      fail verify-static
    fi
  done
fi

step "tar $TARBALL"
tar --sort=name --owner=0 --group=0 --numeric-owner \
    --mtime='UTC 1970-01-01' -czf "$TARBALL" "$ROOT"

ABS_TARBALL="$(cd "$REPO_ROOT" && pwd)/$TARBALL"
echo "package: $ABS_TARBALL ($(du -h "$TARBALL" | cut -f1))"

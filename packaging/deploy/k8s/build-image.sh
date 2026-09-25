#!/usr/bin/env bash
# 造 gse-agent 的容器镜像，可选直接送进 k3s 的 containerd。
#
# 用法：
#   build-image.sh --pkg <发布包解压目录> [--version <tag>] [--import <ssh-host>] [--dry-run]
#
# 例：
#   packaging/deploy/k8s/build-image.sh --pkg /tmp/vectorman-1.2.0 --version 1.2.0
#   packaging/deploy/k8s/build-image.sh --pkg /tmp/vectorman-1.2.0 --version 1.2.0 --import cloud3
#
# 说明：单机 k3s（如 cloud3）通常没有 registry，用 `docker save | ssh <host> k3s ctr images import -`
# 是 k3s 官方支持的导入方式；集群里若有 registry，请自行 tag/push 并改清单里的 image。
set -euo pipefail

USAGE="usage: build-image.sh --pkg <解压后的发布包目录> [--version <tag>] [--import <ssh-host>] [--dry-run]"

PKG=""
VERSION="dev"
IMPORT_HOST=""
DRY_RUN=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --pkg) PKG="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --import) IMPORT_HOST="$2"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) echo "$USAGE"; exit 0 ;;
    *) echo "$USAGE" >&2; exit 2 ;;
  esac
done

if [[ -z "$PKG" ]]; then
  echo "$USAGE" >&2
  exit 2
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
DOCKERFILE="$REPO_ROOT/packaging/deploy/k8s/Dockerfile"
IMAGE="vectorman-gse-agent:$VERSION"

BIN="$PKG/gse-agent/bin/gse-agent"
if [[ ! -f "$BIN" ]]; then
  echo "找不到 Agent 二进制：$BIN（--pkg 要指向发布包解压后的根目录）" >&2
  exit 1
fi
# 静态链接是 scratch 镜像的前提：动态链接进 scratch 会因为缺 loader 起不来。
if command -v file >/dev/null 2>&1; then
  if ! file "$BIN" | grep -q "statically linked"; then
    echo "警告：$BIN 看起来不是静态链接，scratch 镜像可能起不来：" >&2
    file "$BIN" >&2
  fi
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "需要 docker 来 build/save；也可用 buildah/nerdctl 自行完成等价的构建与导入。" >&2
  exit 1
fi

echo "==> 构建 $IMAGE（上下文 $PKG）"
if [[ "$DRY_RUN" -eq 1 ]]; then
  echo "dry-run: docker build -f $DOCKERFILE -t $IMAGE $PKG"
else
  docker build -f "$DOCKERFILE" -t "$IMAGE" "$PKG"
fi

if [[ -n "$IMPORT_HOST" ]]; then
  # containerd 的 socket 只有 root 能连：能连就用 k3s ctr，否则用 sudo -n k3s ctr。
  CTR="k3s ctr"
  if ! ssh "$IMPORT_HOST" 'k3s ctr images ls >/dev/null 2>&1'; then
    if ssh "$IMPORT_HOST" 'sudo -n true 2>/dev/null'; then
      CTR="sudo -n k3s ctr"
    else
      echo "$IMPORT_HOST 上既不能直连 containerd，也没有免密 sudo；请用 root ssh 或手动导入" >&2
      exit 1
    fi
  fi
  echo "==> 导入 $IMPORT_HOST 的 k3s containerd（$CTR）"
  if [[ "$DRY_RUN" -eq 1 ]]; then
    echo "dry-run: docker save $IMAGE | ssh $IMPORT_HOST '$CTR images import -'"
  else
    docker save "$IMAGE" | ssh "$IMPORT_HOST" "$CTR images import -"
    ssh "$IMPORT_HOST" "$CTR images ls | grep -F '$IMAGE' || { echo '导入后没查到镜像' >&2; exit 1; }"
    echo "==> 已导入；接着 apply 清单（记得把 image 换成 $IMAGE）"
  fi
fi

echo "==> 完成：$IMAGE"

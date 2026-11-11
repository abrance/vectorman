#!/usr/bin/env bash
# 集成测试统一入口（研发规范 §4 的 Cargo 版本）。
#
# 设计原则：**缺东西就死，不静默跳过**。一个「因为没配 .env 所以绿了」的集成测试
# 比没有集成测试更糟——它给出的信号是假的。所以这里所有前置条件都用 die() 退出，
# 并且必须非零退出码 + 能照着做的修复提示。
#
# 用法：
#   cp .env.integration.example .env.integration   # 按本机实际情况改地址
#   scripts/test-integration.sh
#
# 退出码：0 = 全部通过；1 = 前置条件不满足或有用例失败。
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

die() {
    printf '\n[test-integration] 失败：%s\n' "$1" >&2
    shift
    for line in "$@"; do printf '  → %s\n' "$line" >&2; done
    printf '\n' >&2
    exit 1
}

step() { printf '\n[test-integration] %s\n' "$*"; }

# ---------------------------------------------------------------------------
# 1. 环境文件
# ---------------------------------------------------------------------------
ENV_FILE="${TEST_INTEGRATION_ENV_FILE:-.env.integration}"
if [ ! -f "$ENV_FILE" ]; then
    die "找不到环境文件 $ENV_FILE" \
        "先复制一份模板：cp .env.integration.example .env.integration" \
        "然后把里面的 Redis / VictoriaMetrics 地址改成你本机（或 CI 里）真实可用的地址"
fi

# shellcheck disable=SC1090
set -a; . "$ENV_FILE"; set +a

# ---------------------------------------------------------------------------
# 2. 环境标识：只允许 local/dev/ci
# ---------------------------------------------------------------------------
case "${TEST_ENV:-}" in
    local|dev|ci) ;;
    prod)
        die "TEST_ENV=prod 拒绝执行" \
            "集成测试会往 Redis 写 e2e-<pid>- 前缀的键、往 VM 写测试序列，不允许指向生产" \
            "TEST_ENV 只能是 local / dev / ci"
        ;;
    '')
        die "TEST_ENV 未设置（$ENV_FILE 里）" \
            "写 TEST_ENV=local（或 dev / ci）"
        ;;
    *)
        die "TEST_ENV='${TEST_ENV}' 不认识" \
            "只接受 local / dev / ci"
        ;;
esac

# ---------------------------------------------------------------------------
# 3. 必填变量：为空也算缺（.env 里写了 KEY= 这种情况最容易漏）
# ---------------------------------------------------------------------------
need() {
    local name=$1
    if [ -z "${!name:-}" ]; then
        die "必填环境变量 $name 为空或未设置" \
            "在 $ENV_FILE 里补上 $name=<真实地址>，示例见 .env.integration.example"
    fi
}

need DP_E2E_REDIS_URL
need DP_E2E_VM_INSERT_URL
need DP_E2E_VM_SELECT_URL

# ---------------------------------------------------------------------------
# 4. 连通性：地址写了但服务没起，是最常见的「以为跳过其实没跑」
# ---------------------------------------------------------------------------
tcp_check() {
    local host=$1 port=$2 label=$3 hint=$4
    if ! timeout 3 bash -c "exec 3<>/dev/tcp/${host}/${port}" 2>/dev/null; then
        die "连不通 $label（${host}:${port}）" "$hint"
    fi
}

# redis://[:password@]host:port/db → host port
redis_endpoint() {
    local rest=${1#redis://} hostport
    hostport=${rest%%/*}     # 去掉 /db
    hostport=${hostport##*@} # 去掉 user:password@
    printf '%s %s' "${hostport%%:*}" "${hostport#*:}"
}

# http(s)://host[:port][/path] → host port
http_endpoint() {
    local rest=${1#http://}
    rest=${rest#https://}
    rest=${rest%%/*}
    if [ "$rest" = "${rest#*:}" ]; then
        printf '%s 80' "$rest"
    else
        printf '%s %s' "${rest%%:*}" "${rest##*:}"
    fi
}

read -r REDIS_HOST REDIS_PORT <<<"$(redis_endpoint "$DP_E2E_REDIS_URL")"
[ -n "${REDIS_PORT:-}" ] || REDIS_PORT=6379
tcp_check "$REDIS_HOST" "$REDIS_PORT" "Redis" \
    "起一个：docker run -d --name dp-e2e-redis -p 6379:6379 redis:7-alpine"

for var in DP_E2E_VM_INSERT_URL DP_E2E_VM_SELECT_URL; do
    read -r host port <<<"$(http_endpoint "${!var}")"
    tcp_check "$host" "$port" "VictoriaMetrics（$var）" \
        "起一个单机版：docker run -d --name dp-e2e-vm -p 8428:8428 victoriametrics/victoria-metrics"
done

# ---------------------------------------------------------------------------
# 5. 用例
# ---------------------------------------------------------------------------
step "IT-DS-BOOT-01：dataserver 真进程启动 + 接入 + 回读（不需要外部中间件）"
cargo test -p dataserver --test boot_smoke -- --test-threads=1

step "IT-EXT-REDIS-VM-01：真实 Redis / VictoriaMetrics 后端语义与一致"
cargo test -p dataserver --test external_storage_e2e -- --test-threads=1 --nocapture

step "全部集成用例通过"

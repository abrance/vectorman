#!/usr/bin/env bash
# 升级一个 GSE Agent（一条命令）。
#
# 原理：先把新二进制用 file_transfer 落到目标机，再下发 `agent_upgrade` 作业。
# Agent 受理后**不自己停自己**，而是写一个 cron 一次性任务，由 cron 以独立
# cgroup 拉起升级进程（停 → 备份 → 替换 → 起 → 必要时回滚）。
# 升级结果由重启后的 Agent 在心跳里补报（升级时作业通道已断，只能这样回报）。
#
# 为什么不让脚本自己停 agent：实测三种脱离方式都失败（ctl.sh 直停 /
# setsid 不脱 cgroup / systemd-run 外层仍卡 running），详见
# `.monkeycode/specs/gse-agent-self-update/design.md`。
#
# 用法：
#   scripts/upgrade-agent.sh --agent testbkee --binary ./gse-agent
#   scripts/upgrade-agent.sh --agent testbkee --from-release v1.1.0 --wait
#   scripts/upgrade-agent.sh --agent testbkee --status
#
# 前置：目标 agent 的作业通道可用（脚本会先核对，不可用则拒绝下发）。
set -euo pipefail

url="https://vectorman.xiaoyxq.top"
vmctl="${VMCTL:-/opt/vectorman/vmctl/bin/vmctl}"
agent=""
binary=""
from_release=""
wait_flag=0
status_only=0

usage() {
  awk 'NR==1{next} /^#/{sub(/^# ?/,""); print; next} {exit}' "${BASH_SOURCE[0]}"
  exit "${1:-0}"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --agent) agent="${2:-}"; shift 2 ;;
    --binary) binary="${2:-}"; shift 2 ;;
    --from-release) from_release="${2:-}"; shift 2 ;;
    --url) url="${2:-}"; shift 2 ;;
    --vmctl) vmctl="${2:-}"; shift 2 ;;
    --wait) wait_flag=1; shift ;;
    --status) status_only=1; shift ;;
    -h|--help) usage 0 ;;
    *) echo "未知参数: $1" >&2; usage 1 ;;
  esac
done

[ -n "$agent" ] || { echo "缺少 --agent" >&2; usage 1; }

check_py="$(mktemp)"
trap 'rm -f "$check_py"' EXIT
cat > "$check_py" <<'PY'
import json, os, sys
agent = os.environ["AGENT"]
try:
    rows = json.load(sys.stdin)
except json.JSONDecodeError:
    print("台账响应不是 JSON", file=sys.stderr); sys.exit(2)
row = next((r for r in rows if r.get("agent_id") == agent), None)
if row is None:
    print(f"台账里没有 agent {agent}", file=sys.stderr); sys.exit(2)
print(f"    status={row.get('status')} session_state={row.get('session_state')} "
      f"job_channel_available={row.get('job_channel_available')}")
sys.exit(0 if row.get("job_channel_available") else 3)
PY

agent_state() {
  curl -sS -m 10 "$url/api/gse/agents" | AGENT="$agent" python3 "$check_py"
}

if [ "$status_only" = 1 ]; then
  agent_state
  exit 0
fi

[ -n "$binary" ] || [ -n "$from_release" ] || {
  echo "需要 --binary <本地文件> 或 --from-release <tag>" >&2
  usage 1
}

# ── 1. 取二进制 ──────────────────────────────────────────────────────────────
work="$(mktemp -d)"
trap 'rm -rf "$work"; rm -f "$check_py"' EXIT
bin_path=""
if [ -n "$binary" ]; then
  [ -f "$binary" ] || { echo "找不到二进制: $binary" >&2; exit 1; }
  bin_path="$binary"
else
  echo "==> 从 release $from_release 下载 agent 二进制"
  tarball="vectorman-$from_release-linux-x86_64.tar.gz"
  gh release download "$from_release" --repo abrance/vectorman --pattern "$tarball" -D "$work" >/dev/null
  tar xzf "$work/$tarball" -C "$work"
  bin_path="$work/vectorman-$from_release-linux-x86_64/gse-agent/bin/gse-agent"
  [ -f "$bin_path" ] || { echo "压缩包里没有 gse-agent" >&2; exit 1; }
fi
sha="$(sha256sum "$bin_path" | cut -d' ' -f1)"
echo "==> 待升二进制: $bin_path"
echo "    版本: $("$bin_path" --version 2>/dev/null || echo '(取不到)')"
echo "    sha256: $sha"

# ── 2. 核对作业通道 ──────────────────────────────────────────────────────────
echo "==> 核对 $agent 的会话状态"
agent_state || {
  echo "    作业通道不可用，无法下发升级 —— 需要人工登机处理" >&2
  exit 1
}

# ── 3. 传二进制到目标机（必须 --wait，否则升级作业会找不到文件）──────────────
remote="/tmp/gse-agent-new-$sha"
echo "==> 传输二进制到 $agent:$remote"
# `|| true`：vmctl 在作业失败时仍可能以 0 退出，但若它以非 0 退出，
# `set -e` 会在下面的判断之前就把脚本杀掉（拿不到可读错误）。
transfer="$("$vmctl" --url "$url" jobs submit --kind file_transfer \
  --upload "$bin_path" --to-agent "$agent" --to-path "$remote" --wait 2>&1 || true)"
if printf '%s' "$transfer" | grep -q '"status":"succeeded"'; then
  echo "    传输完成"
elif printf '%s' "$transfer" | grep -q '"error":"already_exists"'; then
  # 远端路径由 sha 派生，同名文件必然是同一份内容 → 直接复用。
  # （file_transfer 不覆盖已有文件，重复升级同一版本时会走到这里。）
  echo "    目标机已有同 sha 的二进制，复用"
else
  echo "传输失败: $transfer" >&2
  exit 1
fi

# ── 4. 下发升级作业 ──────────────────────────────────────────────────────────
echo "==> 下发 agent_upgrade 作业"
result="$("$vmctl" --url "$url" jobs submit --kind agent_upgrade \
  --agent-id "$agent" --binary-path "$remote" --sha256 "$sha" --wait 2>&1)"
printf '%s\n' "$result" | python3 -c '
import json, sys
d = json.load(sys.stdin)
print("    受理:", d.get("status"), "|", d.get("reason") or d.get("error") or "")
' 2>/dev/null || echo "    $result"

echo
echo "==> 升级已交给目标机的 cron（下一分钟内执行）。"
echo "    观察：scripts/upgrade-agent.sh --agent $agent --status"
echo "    结果：agent 重启后会在心跳里带上 outcome（server 日志/台账可查）"
if [ "$wait_flag" = 1 ]; then
  echo "==> 轮询会话恢复（最多 5 分钟）"
  for _ in $(seq 1 60); do
    sleep 5
    if agent_state >/dev/null 2>&1; then
      echo "    $agent 会话已恢复"
      exit 0
    fi
  done
  echo "    ⚠️ 超时未恢复 —— 登机查看 /tmp/gse-agent-upgrade.log 与升级结果文件" >&2
  exit 1
fi

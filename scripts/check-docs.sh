#!/usr/bin/env bash
# 文档相对链接存在性检查（只查 git 跟踪的 .md）。
# 用法：scripts/check-docs.sh   —— 全绿退出 0；死链打印 DEAD 行并退出 1。
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

dead=""
while IFS= read -r f; do
  dir=$(dirname "$f")
  # 抓 [..](path#anchor) 形式的相对链接（跳过 http/mailto/纯锚点）
  while IFS= read -r link; do
    case "$link" in
      http*|mailto:*|\#*) continue ;;
    esac
    path="${link%%#*}"
    [ -z "$path" ] && continue
    path=${path//\`/}
    if [ ! -e "$dir/$path" ] && [ ! -e "$path" ]; then
      dead+="DEAD: $f -> $link"$'\n'
    fi
  done < <(grep -oE '\]\([^)]+\)' "$f" | sed 's/^](//;s/)$//')
done < <(git -c core.quotepath=false ls-files '*.md')

if [ -n "$dead" ]; then
  printf '%s' "$dead" >&2
  echo "文档死链检查：未通过" >&2
  exit 1
fi
echo "文档死链检查：通过"

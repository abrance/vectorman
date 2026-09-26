#!/usr/bin/env python3
"""归档压缩 .monkeycode/specs 下的 19 个已完结 spec 的 requirements.md。

规则（对应 PR 里的人工核对）：
- ACTIVE（gse-session-liveness / ebpf-observability / observability-hardening）不碰。
- 每个被压缩的 Requirement 保留编号 + 标题 + User Story + 一行验收摘要。
- 摘要 = Acceptance Criteria 里前 1-2 条 EARS 条款压成一行（保留 SHALL/MUST 语义词与关键标识符）。
- 源码注释按「requirements.md Requirement N」引用 → 编号绝对不动，文件名不动。
"""
import re, sys, pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent / ".monkeycode" / "specs"
ACTIVE = {"gse-session-liveness", "ebpf-observability", "observability-hardening"}
ARCHIVE_NOTE = (
    "> **已归档**（2026-09-26 文档整理）：本 feature 已实现并合入 main，本文件压缩为需求索引——"
    "完整 EARS 条款见 git 历史（本文件重写前的最后一个版本），design.md 全文保留为实施细节档案。\n"
)

def one_line(ac: str) -> str:
    """把一组 EARS 条款压成一行摘要：保留前两条的主干。"""
    items = [x.strip() for x in ac.strip().splitlines() if x.strip()]
    keep = []
    for it in items[:2]:
        # 去掉序号，压掉「THE xxx SHALL」之外的修饰
        it = re.sub(r"^\d+\.\s*", "", it)
        it = re.sub(r"\s+", " ", it).rstrip("。")
        keep.append(it)
    return "；".join(keep) + "。"

def compress(path: pathlib.Path) -> None:
    text = path.read_text(encoding="utf-8")
    # 切分 Requirement 块：### Requirement N[: 标题]
    blocks = re.split(r"(?m)(?=^### Requirement \d+)", text)
    head = blocks[0]
    reqs = blocks[1:]
    out = [head]
    for b in reqs:
        m = re.match(r"### (Requirement \d+[:：]?[^#\n]*)", b)
        title = m.group(1).strip() if m else b.splitlines()[0]
        us = re.search(r"\*\*User Story:\*\*(.+)", b)
        us = us.group(1).strip() if us else ""
        ac_m = re.search(r"#### Acceptance Criteria\n(.*?)(?=\n### |\Z)", b, re.S)
        summary = one_line(ac_m.group(1)) if ac_m else "（无验收条款）"
        out.append(f"### {title}\n\n- {us}\n- 验收：{summary}\n")
    body = "".join(out)
    # Introduction 段保留（首个 ## Introduction 到 ## Glossary/Requirements 之间）
    # head 已含。写回：归档注 + 原头部 + 压缩块。
    new = re.sub(
        r"(?m)^(# .*?\n)",
        r"\1\n" + ARCHIVE_NOTE,
        body,
        count=1,
    )
    path.write_text(new, encoding="utf-8")
    print(f"  {path.parent.name}: {len(text)} -> {len(new)} chars")

def main() -> None:
    args = sys.argv[1:]
    dry = "--dry-run" in args
    if [a for a in args if a != "--dry-run"]:
        sys.exit(f"未知参数: {args}（仅支持 --dry-run）")
    done = 0
    for d in sorted(ROOT.iterdir()):
        if not d.is_dir() or d.name in ACTIVE:
            continue
        req = d / "requirements.md"
        if not req.exists():
            continue
        if dry:
            print(f"[dry] {d.name}: 将压缩")
            done += 1
            continue
        print(f"{d.name}:")
        compress(req)
        done += 1
    print(f"\n{'[dry-run] ' if dry else ''}compressed {done} specs")

if __name__ == "__main__":
    main()

# eBPF 内核态程序产物

本目录存放 `crates/gse-ebpf-programs` 构建出的 BPF 目标文件（`network.o`、`tcp.o`、`process.o`）。
用户态 `gse-agent-ebpf` 通过 `build.rs` 在**编译期**内嵌这些文件（存在则 `include_bytes!`，
不存在则返回空切片并在运行时报「先构建」的明确错误），**部署期不编译**。

## 怎么产出

前置：`bpf-linker` + LLVM。注意版本 —— **bpf-linker 0.11 的默认 feature 是 LLVM 23**，
Ubuntu 24.04 自带的 LLVM 18 不够，需要 LLVM 21+（如 `apt.llvm.org` 的 `llvm-21-dev`）：

```bash
LLVM_SYS_211_PREFIX=/usr/lib/llvm-21 \
  cargo install bpf-linker --no-default-features --features llvm-21
scripts/build-ebpf.sh            # 产物落到本目录
scripts/build-ebpf.sh --check    # 只做类型检查（不需要 bpf-linker，CI 跑这个）
```

本地没有 LLVM 21 时不必纠结：**手动触发 CI 的 `ebpf-objects` 作业**即可拿到产物
（GitHub → Actions → Rust CI → Run workflow，然后下载 `ebpf-objects` artifact）。

## 为什么不把 `.o` 提交进 git

设计原文写的是「产物入库 `packaging/ebpf/`」，实现改为**构建期产出并内嵌**，理由：

1. 二进制入库会随时间陈旧 —— 源码改了、`.o` 忘了重编，运行的是旧程序且看不出来；
2. 评审噪音大（每次改动产生大体积 diff）；
3. `build.rs` 已经能区分「没有产物」与「内核不支持」：前者报「运行 scripts/build-ebpf.sh」，
   后者是 preflight 的结论，不会互相混淆。

发布流程已经接上：`packaging/build-package.sh` 在本机检测到 `bpf-linker` 时会先构建 `.o` 再打
Rust 二进制，因此**发布包里的 `gse-agent` 自带内嵌对象**；没有 `bpf-linker` 时只跳过这一步并给出提示。

## 特权环境验证（设计里的「检查点」）

加载/挂载/差分读取都无法在普通用户下验证（`bpf_preflight` 要求 root 或 `CAP_BPF`+`CAP_PERFMON`），
因此提供了一个可照抄执行的检查点工具：

```bash
# 1. 产出对象文件（本机或 CI）
scripts/build-ebpf.sh

# 2. 在目标机用 root 跑（会真的 attach，然后每 2 秒读一次 per-CPU 快照）
sudo -E cargo run -p gse-agent-ebpf --example checkpoint -- \
    --object packaging/ebpf/network.o --kind ebpf_network --seconds 15

# 3. 另一个终端制造受控流量（本机自测要带 --include-loopback，因为回环默认丢弃）
curl -s http://127.0.0.1:8080/ >/dev/null
```

退出码：`0` 通过；`2` 前置校验失败；`3` 加载/挂载失败；`4` 读快照失败或全程没采到连接。
输出里会打印解析出的 `CFG` 偏移与每条连接的计数，便于对照内核版本排查偏移问题。

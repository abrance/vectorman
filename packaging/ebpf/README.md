# eBPF 内核态程序产物

本目录存放 `crates/gse-ebpf-programs` 构建出的 BPF 目标文件（`network.o`、`tcp.o`、`process.o`）。
用户态 `gse-agent-ebpf` 通过 `include_bytes!` 嵌入，**部署期不编译**。

生成方式：

```bash
cargo install bpf-linker      # 需要本机 LLVM
scripts/build-ebpf.sh         # 产物落到本目录
```

只做类型检查（不需要 bpf-linker）：`scripts/build-ebpf.sh --check`（CI 跑这个）。

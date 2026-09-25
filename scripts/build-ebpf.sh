#!/usr/bin/env bash
# 构建 eBPF 内核态程序（crates/gse-ebpf-programs）并产出 packaging/ebpf/*.o。
#
# 需要：nightly 工具链（rust-src）、bpf-linker（`cargo install bpf-linker`，依赖 LLVM）。
# 产物入库 `packaging/ebpf/`，部署期不编译（设计 Pitfalls 第一条）。
#
# 只做类型检查（不需要 bpf-linker）：`scripts/build-ebpf.sh --check`
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
crate_dir="$root/crates/gse-ebpf-programs"
out_dir="$root/packaging/ebpf"
target_dir="$root/target/ebpf"
mode="${1:-build}"

if [[ "$mode" == "--check" ]]; then
    echo "==> 类型检查内核态程序（bpfel-unknown-none）"
    cd "$crate_dir"
    CARGO_TARGET_DIR="$target_dir" cargo +nightly check --target bpfel-unknown-none -Z build-std=core --bins
    echo "==> 类型检查通过（未产出 .o；产 .o 需要 bpf-linker）"
    exit 0
fi

if ! command -v bpf-linker >/dev/null 2>&1; then
    cat >&2 <<'EOF'
错误：缺少 bpf-linker，无法链接出 .o。

  cargo install bpf-linker        # 需要本机 LLVM（可用 LLVM_SYS_*_PREFIX 指定）

或只做类型检查（无需 bpf-linker）：

  scripts/build-ebpf.sh --check
EOF
    exit 1
fi

echo "==> 构建内核态程序"
cd "$crate_dir"
CARGO_TARGET_DIR="$target_dir" cargo +nightly build --release --target bpfel-unknown-none -Z build-std=core --bins

mkdir -p "$out_dir"
for bin in network tcp process; do
    src="$target_dir/bpfel-unknown-none/release/$bin"
    [[ -f "$src" ]] || { echo "错误：缺少构建产物 $src" >&2; exit 1; }
    install -m 0644 "$src" "$out_dir/$bin.o"
    printf '  %s (%s 字节)\n' "$out_dir/$bin.o" "$(stat -c%s "$out_dir/$bin.o")"
done

# 守卫：对象里不能有**未定义的函数符号**。
#
# 踩过的坑：内核态里对 u64 做常量除法（LLVM 会优化成 128 位乘法）或用 `saturating_mul`
# （需要 128 位乘积判溢出），都会引用 compiler_builtins 的 `__multi3`。这种对象能编出来，
# 但 aya 加载时会在函数重定位阶段失败（`error relocating function`），排查成本高。
# 在构建期就挡住，比在目标机上发现便宜得多。
for bin in network tcp process; do
    undefined="$(llvm-readelf -s "$out_dir/$bin.o" 2>/dev/null |
        awk '$4=="FUNC" && $7=="UND" {print $8}' | tr '\n' ' ')"
    if [[ -n "$undefined" ]]; then
        echo "错误：$out_dir/$bin.o 引用了未定义的函数符号：$undefined" >&2
        echo "      通常是内核态里出现了常量除法（u64 / 常量）或 saturating_mul —— 改用移位/普通乘法。" >&2
        exit 1
    fi
done
echo "==> 完成：$out_dir（无未定义函数符号）"

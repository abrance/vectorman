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
echo "==> 完成：$out_dir"

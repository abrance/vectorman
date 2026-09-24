//! 把 `packaging/ebpf/*.o` 嵌入用户态（设计 Pitfalls 第一条：部署期不编译 eBPF）。
//!
//! 生成的 `ebpf_objects.rs` 在文件存在时用 `include_bytes!`（绝对路径），不存在时返回空切片：
//! 这样**没有 `.o` 的仓库也能正常编译**（运行时给出「先跑 scripts/build-ebpf.sh」的明确错误），
//! 而不像直接 `include_bytes!` 那样编译失败。

use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let dir = manifest
        .join("..")
        .join("..")
        .join("packaging")
        .join("ebpf");
    println!("cargo:rerun-if-changed={}", dir.display());

    let mut out = String::from("// 由 build.rs 生成，勿手工修改。\n");
    for (constant, file) in [
        ("NETWORK", "network.o"),
        ("TCP", "tcp.o"),
        ("PROCESS", "process.o"),
    ] {
        let path = dir.join(file);
        println!("cargo:rerun-if-changed={}", path.display());
        if path.exists() {
            out.push_str(&format!(
                "/// 内嵌的 eBPF 目标文件（{}）。\npub const {}: &[u8] = include_bytes!({:?});\n",
                file,
                constant,
                path.to_string_lossy()
            ));
        } else {
            out.push_str(&format!(
                "/// eBPF 目标文件缺失（{}）；运行 scripts/build-ebpf.sh 后重新编译。\npub const {}: &[u8] = &[];\n",
                file, constant
            ));
        }
    }

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let dest = Path::new(&out_dir).join("ebpf_objects.rs");
    std::fs::write(&dest, out).expect("写 ebpf_objects.rs");
}

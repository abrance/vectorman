//! 打印自更新的调度脚本（dry-run），用于人工审查或手动演练。
//!
//! 用法：
//!   cargo run -p gse-agent-core --example render-upgrade-script -- <新二进制路径>
//!
//! 输出即 `accept_upgrade` 会写进 cron 一次性任务的那个脚本。
//! 运维可以在真机上先看一眼（确认停/起命令、备份路径、回滚分支符合预期），
//! 再让 agent 真正受理升级。

use std::path::Path;

use gse_agent_core::upgrade::{detect_deploy, render_inner_script, systemd_unit_exists};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let new_bin = args
        .first()
        .map(String::as_str)
        .unwrap_or("/tmp/new-gse-agent");

    let Some(deploy) = detect_deploy(|p| Path::new(p).exists(), systemd_unit_exists) else {
        eprintln!("找不到已安装的 gse-agent —— 无法探测部署形式");
        std::process::exit(1);
    };

    eprintln!("# 部署形式: {:?}", deploy.kind);
    eprintln!("# 被替换: {}", deploy.bin.display());
    eprintln!("# ctl.sh: {:?}", deploy.ctl);
    eprintln!(
        "# 结果文件: {}",
        gse_agent_core::upgrade::result_file_path().display()
    );
    eprintln!("# ─────────────────────────────────────────");
    print!(
        "{}",
        render_inner_script(&deploy, Path::new(new_bin), "DRYRUN")
    );
}

//! Agent 自更新：受理升级请求、调度独立进程、记录并回报结果。
//!
//! 为什么不能「在 agent 进程里停自己换二进制」：停 agent 会连带杀掉正在
//! 执行升级的一切（作业子进程、外层脚本），升级必然半途而废。实测三种脱离
//! 方式都不成立：
//!
//! - `ctl.sh stop` 直接停 → 作业是 agent 子进程，被连带杀
//! - `setsid nohup` → 只脱**进程组**、**不脱 cgroup**；systemd 按 cgroup 杀全部
//! - `systemd-run --unit` → 内层确实独立了，但**外层作业仍卡 `running`**
//!   （外层在 agent 的 cgroup 里等内层）
//!
//! 因此实际执行升级的动作交给 **cron 一次性任务**：`cron.service` 是独立
//! 的 cgroup，父进程是 cron 而非 agent，agent 停/起都不影响它。
//!
//! 本模块只做「受理 + 生成调度脚本 + 结果读写」；真正的替换与重启由生成的
//! shell 脚本在 cron 拉起时执行。

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// 部署形式与关键路径，由 [`detect_deploy`] 探测得出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deploy {
    pub kind: DeployKind,
    /// 已安装的 agent 二进制（要被替换的那个）。
    pub bin: PathBuf,
    /// `ctl.sh` 路径；systemd 部署下也可能存在（备份用不到，保留便于人工恢复）。
    pub ctl: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployKind {
    /// systemd unit `vectorman-gse-agent.service`：用 `systemctl stop/start`。
    Systemd,
    /// `ctl.sh` 的 direct 模式（无 systemd）：用 `ctl.sh gse-agent stop/start`。
    CtlDirect,
}

/// 候选路径：按顺序取第一个存在的。两个部署形式的布局都在候选里。
const BIN_CANDIDATES: &[&str] = &[
    "/home/test/dtx/vectorman/gse-agent/bin/gse-agent",
    "/opt/vectorman/gse-agent/bin/gse-agent",
];

const CTL_CANDIDATES: &[&str] = &[
    "/home/test/dtx/vectorman/deploy/ctl.sh",
    "/opt/vectorman/deploy/ctl.sh",
];

/// systemd unit 名（存在即认为是 systemd 部署）。
pub const SYSTEMD_UNIT: &str = "vectorman-gse-agent.service";

/// 探测部署形式。
///
/// `exists` 与 `has_systemd_unit` 是注入的判定函数，使本函数可测
/// （真实实现读文件系统 / 查 systemd）。
pub fn detect_deploy(
    exists: impl Fn(&str) -> bool,
    has_systemd_unit: impl Fn(&str) -> bool,
) -> Option<Deploy> {
    let bin = BIN_CANDIDATES
        .iter()
        .find(|p| exists(p))
        .map(PathBuf::from)?;
    let ctl = CTL_CANDIDATES.iter().find(|p| exists(p)).map(PathBuf::from);
    let kind = if has_systemd_unit(SYSTEMD_UNIT) {
        DeployKind::Systemd
    } else {
        DeployKind::CtlDirect
    };
    Some(Deploy { kind, bin, ctl })
}

/// 生成备份路径：`<bin>.bak-<时间戳>`。
///
/// 时间戳精确到纳秒并附序号，保证「同一秒内多次调用也不互相覆盖」。
pub fn plan_backup(bin: &Path, stamp: &str, seq: u32) -> PathBuf {
    let mut name = bin.as_os_str().to_os_string();
    name.push(format!(".bak-{stamp}-{seq}"));
    PathBuf::from(name)
}

/// sha256 校验。`expected` 为空视为不校验（调用方自行决定是否允许）。
pub fn verify_sha256(actual: &str, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    actual.eq_ignore_ascii_case(expected)
}

/// 升级结果。落本机文件，新 agent 启动后在心跳里带出。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpgradeResult {
    pub started_at: String,
    pub finished_at: String,
    pub from_version: String,
    pub to_version: String,
    pub from_sha256: String,
    pub to_sha256: String,
    pub outcome: UpgradeOutcome,
    /// 失败/回滚时的原因。
    #[serde(default)]
    pub detail: String,
    /// 是否已在心跳中上报过（上报后置 true；文件保留便于事后查证）。
    #[serde(default)]
    pub reported: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeOutcome {
    /// 替换后新二进制启动成功。
    Succeeded,
    /// 新二进制起不来，已用备份回滚并重启。
    RolledBack,
    /// 失败且回滚也未成功（现场保留，需人工介入）。
    Failed,
}

/// 结果文件的默认位置（放安装目录，随 agent 的 data 一起）。
pub fn default_result_path(bin: &Path) -> PathBuf {
    bin.parent()
        .and_then(|p| p.parent())
        .map(|dir| dir.join("upgrade-result.json"))
        .unwrap_or_else(|| PathBuf::from("/tmp/gse-agent-upgrade-result.json"))
}

/// 等待「新二进制启动成功」的窗口。
pub const STARTUP_GRACE: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use super::*;

    fn only(path: &str) -> impl Fn(&str) -> bool + '_ {
        move |p| p == path
    }

    #[test]
    fn detect_prefers_ctl_layout_when_present() {
        let d = detect_deploy(
            only("/home/test/dtx/vectorman/gse-agent/bin/gse-agent"),
            |_| false,
        )
        .expect("deploy");
        assert_eq!(d.kind, DeployKind::CtlDirect, "无 systemd unit → ctl 模式");
        assert_eq!(
            d.bin,
            PathBuf::from("/home/test/dtx/vectorman/gse-agent/bin/gse-agent")
        );
    }

    #[test]
    fn detect_uses_systemd_when_unit_exists() {
        let d = detect_deploy(only("/opt/vectorman/gse-agent/bin/gse-agent"), |u| {
            u == SYSTEMD_UNIT
        })
        .expect("deploy");
        assert_eq!(d.kind, DeployKind::Systemd);
        assert_eq!(d.bin, PathBuf::from("/opt/vectorman/gse-agent/bin/gse-agent"));
    }

    #[test]
    fn detect_returns_none_without_any_binary() {
        assert!(detect_deploy(|_| false, |_| false).is_none());
    }

    #[test]
    fn detect_finds_ctl_script_when_present() {
        let d = detect_deploy(
            |p| {
                p == "/opt/vectorman/gse-agent/bin/gse-agent"
                    || p == "/opt/vectorman/deploy/ctl.sh"
            },
            |_| false,
        )
        .expect("deploy");
        assert_eq!(d.ctl, Some(PathBuf::from("/opt/vectorman/deploy/ctl.sh")));
    }

    #[test]
    fn backup_path_does_not_collide_within_same_stamp() {
        let bin = Path::new("/opt/vectorman/gse-agent/bin/gse-agent");
        let a = plan_backup(bin, "20260927-180000", 0);
        let b = plan_backup(bin, "20260927-180000", 1);
        assert_ne!(a, b, "同一时间戳的两次备份不能互相覆盖");
        assert!(a.to_string_lossy().ends_with(".bak-20260927-180000-0"));
    }

    #[test]
    fn backup_path_keeps_binary_prefix() {
        let bin = Path::new("/opt/vectorman/gse-agent/bin/gse-agent");
        let b = plan_backup(bin, "T", 0);
        assert!(b.to_string_lossy().starts_with("/opt/vectorman/gse-agent/bin/gse-agent"));
    }

    #[test]
    fn verify_sha256_accepts_case_insensitive_match() {
        let h = "56ca0df6536268a9c59b2dd199c3be5049ee95be97ba5d7d0a660f84d31333fe";
        assert!(verify_sha256(h, h));
        assert!(verify_sha256(h, &h.to_uppercase()));
    }

    #[test]
    fn verify_sha256_rejects_mismatch_and_empty() {
        let h = "56ca0df6536268a9c59b2dd199c3be5049ee95be97ba5d7d0a660f84d31333fe";
        assert!(!verify_sha256(h, "deadbeef"));
        assert!(!verify_sha256(h, ""), "空期望值不得视为通过");
        assert!(!verify_sha256("", h));
    }

    #[test]
    fn upgrade_result_roundtrips() {
        let r = UpgradeResult {
            started_at: "t0".into(),
            finished_at: "t1".into(),
            from_version: "1.1.0".into(),
            to_version: "1.2.0".into(),
            from_sha256: "a".into(),
            to_sha256: "b".into(),
            outcome: UpgradeOutcome::RolledBack,
            detail: "新二进制未启动".into(),
            reported: false,
        };
        let json = serde_json::to_string(&r).expect("encode");
        let back: UpgradeResult = serde_json::from_str(&json).expect("decode");
        assert_eq!(back, r);
        assert!(json.contains("\"rolled_back\""), "outcome 用 snake_case");
    }

    #[test]
    fn upgrade_result_detail_and_reported_default_when_absent() {
        // 旧版本/精简写入时缺字段也要能解析（向后兼容）。
        let json = r#"{"started_at":"t0","finished_at":"t1","from_version":"a",
            "to_version":"b","from_sha256":"x","to_sha256":"y","outcome":"succeeded"}"#;
        let r: UpgradeResult = serde_json::from_str(json).expect("decode");
        assert_eq!(r.detail, "");
        assert!(!r.reported);
    }

    #[test]
    fn default_result_path_sits_under_install_root() {
        let bin = Path::new("/opt/vectorman/gse-agent/bin/gse-agent");
        assert_eq!(
            default_result_path(bin),
            PathBuf::from("/opt/vectorman/gse-agent/upgrade-result.json")
        );
    }
}

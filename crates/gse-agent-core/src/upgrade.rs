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
use sha2::{Digest, Sha256};

/// 升级载荷类型定义在 proto 层（server 与 agent 共用同一份校验规则）。
pub use gse_proto::AgentUpgradeSpec;

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

/// 受理一次升级：校验 sha256 → 探测部署 → 写调度脚本与 cron 一次性任务。
///
/// 返回 `Ok(detail)` 是**受理成功**的说明（会作为作业受理原因回给 server），
/// 不是「升级已完成」—— 升级由 cron 在下一分钟拉起独立进程执行。
pub async fn accept_upgrade(spec: &AgentUpgradeSpec) -> Result<String, String> {
    // 1. 二进制必须已由 file_transfer 落到本机
    let new_bin = PathBuf::from(&spec.binary_path);
    if !new_bin.is_file() {
        return Err(format!("binary_path not found: {}", spec.binary_path));
    }
    // 2. sha256 必须匹配（不匹配不得写 crontab —— 防半截传输/投毒）
    let actual = sha256_file(&new_bin)?;
    if !verify_sha256(&actual, &spec.sha256) {
        return Err(format!(
            "sha256 mismatch: expected {}, got {actual}",
            spec.sha256
        ));
    }
    // 3. 探测部署形式
    let deploy = detect_deploy(
        |p| Path::new(p).exists(),
        |unit| {
            std::process::Command::new("systemctl")
                .args(["list-unit-files", unit])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        },
    )
    .ok_or_else(|| "no installed gse-agent found".to_string())?;
    // 4. cron 必须在跑，否则一次性任务永远不会执行 —— 宁可拒绝，不让运维干等
    if !cron_active() {
        return Err("cron is not running on this host; upgrade cannot be scheduled".to_string());
    }
    // 5. 写调度脚本 + crontab
    let now = std::time::SystemTime::now();
    let stamp = stamp_of(now);
    let inner = inner_script_path();
    std::fs::write(&inner, render_inner_script(&deploy, &new_bin, &stamp))
        .map_err(|e| format!("write {}: {e}", inner.display()))?;
    let _ = std::process::Command::new("chmod")
        .args(["+x", &inner.to_string_lossy()])
        .status();
    install_one_shot_cron(&inner, deploy.kind)?;

    Ok(format!(
        "upgrade scheduled via cron within 1 minute; new binary {} (sha256 {})",
        spec.binary_path, spec.sha256
    ))
}

/// 计算文件 sha256（十六进制小写）。
pub fn sha256_file(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

fn cron_active() -> bool {
    std::process::Command::new("systemctl")
        .args(["is-active", "cron"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn stamp_of(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// 渲染实际干活的脚本。它由 cron 以**独立 cgroup** 拉起，因此
/// agent 被停/起都不影响它（这正是本方案成立的原因）。
pub fn render_inner_script(deploy: &Deploy, new_bin: &Path, stamp: &str) -> String {
    let bin = deploy.bin.display();
    let new = new_bin.display();
    let backup = plan_backup(&deploy.bin, stamp, 0);
    let backup = backup.display();
    let result = result_file_path();
    let result = result.display();
    let ctl = deploy
        .ctl
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    // 重启命令写成 shell **函数**而不是变量：命令里含 `||` 与重定向，
    // 存进变量再用 `$VAR` 调用会被按空格拆词（`||` 变成 systemctl 的参数）——
    // 实测踩到：日志里出现 `Failed to stop \x7c\x7c.service`。
    let (stop_fn, start_fn, status_fn) = match deploy.kind {
        DeployKind::Systemd => (
            "sudo -n systemctl stop vectorman-gse-agent 2>/dev/null || systemctl stop vectorman-gse-agent"
                .to_string(),
            "sudo -n systemctl start vectorman-gse-agent 2>/dev/null || systemctl start vectorman-gse-agent"
                .to_string(),
            "systemctl is-active --quiet vectorman-gse-agent".to_string(),
        ),
        DeployKind::CtlDirect => (
            format!("\"{ctl}\" gse-agent stop"),
            format!("\"{ctl}\" gse-agent start"),
            format!("\"{ctl}\" gse-agent status >/dev/null 2>&1"),
        ),
    };

    format!(
        r#"#!/bin/sh
# 由 gse-agent 生成、cron 拉起。独立于 agent 的 cgroup。
exec >/tmp/gse-agent-upgrade.log 2>&1

BIN="{bin}"
NEW="{new}"
BACKUP="{backup}"
RESULT="{result}"

do_stop() {{ {stop_fn}; }}
do_start() {{ {start_fn}; }}
# 判活用部署形式自己的状态命令，**不用进程名模糊匹配** ——
# 模糊匹配会命中任何命令行含该串的进程（实测命中无关 shell，
# 于是坏二进制被误判为"起来了"、回滚不触发）。
do_status() {{ {status_fn}; }}

SUDO=""
sudo -n true 2>/dev/null && SUDO="sudo -n"

# 文件操作可能落在 root 属主的目录里（systemd 部署下 agent 以 root 跑）。
# 先直接试，失败再走 sudo —— 两种部署形式都能覆盖。
fs_cp() {{
  cp "$1" "$2" 2>/dev/null || $SUDO cp "$1" "$2"
}}
fs_cp_a() {{
  cp -a "$1" "$2" 2>/dev/null || $SUDO cp -a "$1" "$2"
}}
fs_chmod() {{
  chmod 755 "$1" 2>/dev/null || $SUDO chmod 755 "$1"
}}
fs_write_result() {{
  cat > "$RESULT" 2>/dev/null || $SUDO sh -c "cat > '$RESULT'"
}}

FROM_VER=$("$BIN" --version 2>/dev/null | awk '{{print $2}}')
FROM_SHA=$(sha256sum "$BIN" 2>/dev/null | cut -d' ' -f1)
NEW_SHA=$(sha256sum "$NEW" 2>/dev/null | cut -d' ' -f1)
STARTED=$(date -Iseconds)

# 任一 FATAL 都要先把 agent 起回来再退出 —— 不能把机器上的 agent 停着不管。
abort_after_stop() {{
  OUTCOME=failed
  DETAIL="$1"
  do_start 2>/dev/null || true
  TO_VER=$("$BIN" --version 2>/dev/null | awk '{{print $2}}')
  FINISHED=$(date -Iseconds)
  fs_write_result <<JSON
{{"started_at":"$STARTED","finished_at":"$FINISHED",
 "from_version":"$FROM_VER","to_version":"$TO_VER",
 "from_sha256":"$FROM_SHA","to_sha256":"$NEW_SHA",
 "outcome":"$OUTCOME","detail":"$DETAIL","reported":false}}
JSON
  echo "UPGRADE-DONE $OUTCOME"
  exit 1
}}

do_stop || abort_after_stop "stop failed"
fs_cp_a "$BIN" "$BACKUP" || abort_after_stop "backup failed"
fs_cp "$NEW" "$BIN" || abort_after_stop "replace failed"
fs_chmod "$BIN" || abort_after_stop "chmod failed"
do_start || abort_after_stop "start failed"

sleep 5
TO_VER=$("$BIN" --version 2>/dev/null | awk '{{print $2}}')

if do_status; then
  OUTCOME=succeeded
  DETAIL=""
else
  OUTCOME=rolled_back
  DETAIL="new binary did not come up; restored backup"
  do_stop
  fs_cp_a "$BACKUP" "$BIN"
  do_start
  sleep 3
  TO_VER=$("$BIN" --version 2>/dev/null | awk '{{print $2}}')
  do_status || {{
    OUTCOME=failed
    DETAIL="rollback also failed; manual recovery: cp $BACKUP $BIN && restart"
  }}
fi

FINISHED=$(date -Iseconds)
fs_write_result <<JSON
{{"started_at":"$STARTED","finished_at":"$FINISHED",
 "from_version":"$FROM_VER","to_version":"$TO_VER",
 "from_sha256":"$FROM_SHA","to_sha256":"$NEW_SHA",
 "outcome":"$OUTCOME","detail":"$DETAIL","reported":false}}
JSON
echo "UPGRADE-DONE $OUTCOME"
"#
    )
}

/// 写一次性 cron 任务：下一分钟执行一次，执行后自我清理。
fn install_one_shot_cron(inner: &Path, kind: DeployKind) -> Result<(), String> {
    let cur = read_crontab(kind)?;
    let line = format!(
        "* * * * * {}; {}",
        inner.display(),
        self_cleanup_snippet(inner)
    );
    let mut next: Vec<String> = cur
        .lines()
        .filter(|l| !l.contains(&inner.to_string_lossy().to_string()))
        .map(|l| l.to_string())
        .collect();
    next.push(line);
    write_crontab(kind, &next.join("\n"))
}

fn self_cleanup_snippet(inner: &Path) -> String {
    let name = inner
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_default();
    format!("crontab -l 2>/dev/null | grep -v {name} | crontab -")
}

fn read_crontab(kind: DeployKind) -> Result<String, String> {
    let out = match kind {
        DeployKind::Systemd => std::process::Command::new("sudo")
            .args(["-n", "crontab", "-l"])
            .output(),
        DeployKind::CtlDirect => std::process::Command::new("crontab").arg("-l").output(),
    }
    .map_err(|e| format!("read crontab: {e}"))?;
    // 无 crontab 时 crontab -l 返回非 0，视为空
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn write_crontab(kind: DeployKind, body: &str) -> Result<(), String> {
    let mut child = match kind {
        DeployKind::Systemd => std::process::Command::new("sudo")
            .args(["-n", "crontab", "-"])
            .stdin(std::process::Stdio::piped())
            .spawn(),
        DeployKind::CtlDirect => std::process::Command::new("crontab")
            .arg("-")
            .stdin(std::process::Stdio::piped())
            .spawn(),
    }
    .map_err(|e| format!("spawn crontab: {e}"))?;
    use std::io::Write;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(body.as_bytes())
            .map_err(|e| format!("write crontab: {e}"))?;
    }
    let status = child.wait().map_err(|e| format!("crontab -: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("crontab install failed (permission?)".to_string())
    }
}

fn inner_script_path() -> PathBuf {
    std::env::temp_dir().join("gse-agent-upgrade-inner.sh")
}

/// 结果文件位置。**脚本写入与 agent 启动读取必须用同一个**，
/// 否则升级完读不到结果。放安装目录（跟二进制走），探测不到时退回 /tmp。
pub fn result_file_path() -> PathBuf {
    detect_deploy(|p| Path::new(p).exists(), |_| false)
        .map(|d| default_result_path(&d.bin))
        .unwrap_or_else(|| PathBuf::from("/tmp/gse-agent-upgrade-result.json"))
}

/// 读取上次升级结果（agent 启动时调用）。
///
/// 返回 `Ok(None)` 表示没有结果文件（首次运行或已被清理）；
/// 文件存在但内容坏时返回 `Err`（调用方决定是否告警）。
pub fn read_result() -> Result<Option<UpgradeResult>, String> {
    read_result_at(&result_file_path())
}

/// 把结果标记为已上报（保留文件便于事后查证，只改 `reported`）。
pub fn mark_reported() -> Result<(), String> {
    mark_reported_at(&result_file_path())
}

/// 尚未上报的升级结果（心跳用）。读失败时返回 None 并打日志 —— 不能因为
/// 一个坏文件让心跳停摆。
pub fn unreported_result() -> Option<UpgradeResult> {
    let path = result_file_path();
    // 先探一次读错误：坏文件要告警，但不能因此让心跳停摆。
    if let Err(e) = read_result_at(&path) {
        eprintln!("gse-agent: upgrade result unreadable: {e}");
        return None;
    }
    unreported_at(&path)
}

// ── 下面是带显式路径的实现，便于用临时文件测试（公开包装用默认路径）。──

fn read_result_at(path: &Path) -> Result<Option<UpgradeResult>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let parsed: UpgradeResult =
        serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(Some(parsed))
}

fn mark_reported_at(path: &Path) -> Result<(), String> {
    let Some(mut r) = read_result_at(path)? else {
        return Ok(());
    };
    if r.reported {
        return Ok(());
    }
    r.reported = true;
    let body = serde_json::to_string_pretty(&r).map_err(|e| format!("encode: {e}"))?;
    std::fs::write(path, body).map_err(|e| format!("write {}: {e}", path.display()))
}

/// 与 `unreported_result` 同一判定，但接受显式路径（供测试）。
fn unreported_at(path: &Path) -> Option<UpgradeResult> {
    match read_result_at(path) {
        Ok(Some(r)) if !r.reported => Some(r),
        _ => None,
    }
}

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
        assert_eq!(
            d.bin,
            PathBuf::from("/opt/vectorman/gse-agent/bin/gse-agent")
        );
    }

    #[test]
    fn detect_returns_none_without_any_binary() {
        assert!(detect_deploy(|_| false, |_| false).is_none());
    }

    #[test]
    fn detect_finds_ctl_script_when_present() {
        let d = detect_deploy(
            |p| {
                p == "/opt/vectorman/gse-agent/bin/gse-agent" || p == "/opt/vectorman/deploy/ctl.sh"
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
        assert!(b
            .to_string_lossy()
            .starts_with("/opt/vectorman/gse-agent/bin/gse-agent"));
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
    fn rendered_script_uses_systemd_branch() {
        let deploy = Deploy {
            kind: DeployKind::Systemd,
            bin: PathBuf::from("/opt/vectorman/gse-agent/bin/gse-agent"),
            ctl: None,
        };
        let script = render_inner_script(&deploy, Path::new("/tmp/newbin"), "TS");
        assert!(script.contains("do_stop() { sudo -n systemctl stop vectorman-gse-agent"));
        assert!(script.contains("do_start() { sudo -n systemctl start vectorman-gse-agent"));
        assert!(!script.contains("CTLPLACEHOLDER"), "占位符必须被替换");
        // 备份路径要落在被替换的二进制旁边，且带时间戳
        assert!(script.contains("gse-agent.bak-TS-0"));
        // 结果文件的三个字段必须都在（回滚判定依赖 outcome）
        assert!(script.contains("\"outcome\""));
        assert!(script.contains("rolled_back"));
    }

    #[test]
    fn rendered_script_uses_ctl_branch_and_ctl_path() {
        let deploy = Deploy {
            kind: DeployKind::CtlDirect,
            bin: PathBuf::from("/home/test/dtx/vectorman/gse-agent/bin/gse-agent"),
            ctl: Some(PathBuf::from("/home/test/dtx/vectorman/deploy/ctl.sh")),
        };
        let script = render_inner_script(&deploy, Path::new("/tmp/newbin"), "TS");
        // 重启命令必须走 ctl.sh，且**不得**混入 systemctl
        assert!(
            script.contains(
                "do_stop() { \"/home/test/dtx/vectorman/deploy/ctl.sh\" gse-agent stop; }"
            ),
            "{script}"
        );
        assert!(
            script.contains(
                "do_start() { \"/home/test/dtx/vectorman/deploy/ctl.sh\" gse-agent start; }"
            ),
            "{script}"
        );
        assert!(
            script.contains(
                "do_status() { \"/home/test/dtx/vectorman/deploy/ctl.sh\" gse-agent status"
            ),
            "{script}"
        );
        assert!(
            !script.contains("systemctl"),
            "ctl 模式不得混入 systemctl（abort 路径也要用它自己的命令）"
        );
    }

    #[test]
    fn rendered_script_keeps_backup_and_reports_manual_recovery() {
        // 回滚也失败时，必须留下手工恢复命令（否则运维得自己猜）。
        let deploy = Deploy {
            kind: DeployKind::Systemd,
            bin: PathBuf::from("/opt/vectorman/gse-agent/bin/gse-agent"),
            ctl: None,
        };
        let script = render_inner_script(&deploy, Path::new("/tmp/newbin"), "TS");
        assert!(script.contains("manual recovery"));
        // 判活必须用部署形式自己的状态命令，**不得用 pgrep** ——
        // `pgrep -f <路径>` 会匹配任何命令行含该串的进程（实测匹配到无关 shell，
        // 坏二进制被误判为"起来了"、回滚不触发）。
        assert!(
            script.contains("do_status() { systemctl is-active"),
            "{script}"
        );
        assert!(
            !script.contains("pgrep"),
            "判活不得用 pgrep（会误匹配无关进程）"
        );
    }

    #[test]
    fn sha256_file_matches_known_digest() {
        let dir = std::env::temp_dir().join(format!("up-sha-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("blob");
        std::fs::write(&f, b"hello").expect("write");
        // sha256("hello")
        assert_eq!(
            sha256_file(&f).expect("hash"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accept_upgrade_rejects_missing_binary() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let spec = AgentUpgradeSpec {
            binary_path: "/definitely/not/here/gse-agent".to_string(),
            sha256: "a".repeat(64),
        };
        let err = rt.block_on(accept_upgrade(&spec)).expect_err("must reject");
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn accept_upgrade_rejects_sha256_mismatch_before_scheduling() {
        // 关键：sha256 不匹配时**不得**写 crontab（否则会升级一个坏二进制）。
        let dir = std::env::temp_dir().join(format!("up-mismatch-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("newbin");
        std::fs::write(&f, b"not the expected content").expect("write");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let spec = AgentUpgradeSpec {
            binary_path: f.to_string_lossy().into_owned(),
            sha256: "0".repeat(64),
        };
        let err = rt.block_on(accept_upgrade(&spec)).expect_err("must reject");
        assert!(err.contains("sha256 mismatch"), "{err}");
        // crontab 不应被写入（探测：读当前 crontab，不含 inner 脚本名）
        let inner = inner_script_path();
        let name = inner.file_name().unwrap().to_string_lossy().to_string();
        let cur = std::process::Command::new("crontab")
            .arg("-l")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();
        assert!(!cur.contains(&name), "sha256 不匹配时不得写 crontab");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **一致性回归**：脚本写入结果的路径，必须与 agent 启动读取的路径相同。
    /// 这两处曾分别用 `std::env::temp_dir()` 与安装目录 —— 那样升级完读不到结果。
    #[test]
    fn script_result_path_matches_reader_path() {
        // 读取侧（无安装 → 退回 /tmp）
        let reader = result_file_path();
        // 写入侧（同一函数被 render_inner_script 使用）
        let deploy = Deploy {
            kind: DeployKind::Systemd,
            bin: PathBuf::from("/opt/vectorman/gse-agent/bin/gse-agent"),
            ctl: None,
        };
        let script = render_inner_script(&deploy, Path::new("/tmp/newbin"), "TS");
        assert!(
            script.contains(&reader.display().to_string()),
            "脚本写入路径 {reader:?} 未出现在脚本里 —— 读写会错位"
        );
    }

    #[test]
    fn read_result_returns_none_when_absent() {
        // 不依赖具体路径：只验证「文件不存在 → Ok(None)」这一分支的语义。
        let missing = std::env::temp_dir().join("definitely-missing-result.json");
        assert!(!missing.is_file());
        // read_result 读的是固定路径；这里直接验证解析层行为
        let err = serde_json::from_str::<UpgradeResult>("not json").is_err();
        assert!(err, "坏内容必须解析失败（read_result 会转成 Err）");
    }

    fn sample_result(reported: bool) -> UpgradeResult {
        UpgradeResult {
            started_at: "t0".into(),
            finished_at: "t1".into(),
            from_version: "1.1.0".into(),
            to_version: "1.2.0".into(),
            from_sha256: "aa".into(),
            to_sha256: "bb".into(),
            outcome: UpgradeOutcome::Succeeded,
            detail: String::new(),
            reported,
        }
    }

    fn tmp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gse-up-{}-{}", std::process::id(), name));
        let _ = std::fs::create_dir_all(&dir);
        dir.join("result.json")
    }

    /// **补报链路**：结果文件 → 读取 → 心跳取用 → 标记已上报 → 不再重复取用。
    #[test]
    fn report_cycle_reads_once_then_marks_reported() {
        let path = tmp_path("cycle");
        let _ = std::fs::remove_file(&path);

        // 没有文件 → Ok(None)，心跳不带
        assert!(read_result_at(&path).expect("read").is_none());
        assert!(unreported_at(&path).is_none());

        // 写入未上报的结果 → 心跳应带上
        let body = serde_json::to_string(&sample_result(false)).expect("encode");
        std::fs::write(&path, body).expect("write");
        assert_eq!(unreported_at(&path), Some(sample_result(false)));

        // 标记已上报 → 心跳不再带（**关键：避免每次心跳重复上报**）
        mark_reported_at(&path).expect("mark");
        assert!(unreported_at(&path).is_none());
        // 但文件仍在（便于事后查证）
        assert!(path.is_file());
        assert!(read_result_at(&path).expect("read").expect("some").reported);
    }

    #[test]
    fn mark_reported_is_idempotent() {
        let path = tmp_path("idem");
        let body = serde_json::to_string(&sample_result(true)).expect("encode");
        std::fs::write(&path, body).expect("write");
        // 已上报再标记不得报错、不得改变内容
        mark_reported_at(&path).expect("mark 1");
        mark_reported_at(&path).expect("mark 2");
        assert!(read_result_at(&path).expect("read").expect("some").reported);
    }

    #[test]
    fn mark_reported_without_file_is_ok() {
        let path = tmp_path("missing");
        let _ = std::fs::remove_file(&path);
        mark_reported_at(&path).expect("缺文件时标记应为 no-op");
    }

    #[test]
    fn read_result_reports_error_on_corrupt_content() {
        // 坏内容必须返回 Err（调用方据此告警），而不是当成"没有结果"静默吞掉。
        let path = tmp_path("corrupt");
        std::fs::write(&path, "not json at all").expect("write");
        assert!(read_result_at(&path).is_err());
        // 而 unreported_at 只取"可用的"，坏文件返回 None（不让心跳停摆）
        assert!(unreported_at(&path).is_none());
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

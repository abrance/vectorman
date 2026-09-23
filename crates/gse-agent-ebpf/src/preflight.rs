//! eBPF 前置校验：内核版本、BTF、权限。
//!
//! 设计（`/.monkeycode/specs/ebpf-observability/design.md`「前置校验（preflight）」）：
//! 最低内核 5.8 + BTF 可用 + root 或 `CAP_BPF`（配合 `CAP_PERFMON`）。校验失败只降级
//! eBPF 能力并在 Agent 日志输出 warn（含建议动作），不阻止 Agent 启动。
//!
//! 读取路径与 `uname` 都可注入，便于在无特权环境里单测。

use std::path::{Path, PathBuf};

/// 最低内核版本（`BPF` 能力位从 5.8 起可用）。
pub const MIN_KERNEL_MAJOR: u32 = 5;
pub const MIN_KERNEL_MINOR: u32 = 8;

/// `CapEff` 里的能力位（Linux `include/uapi/linux/capability.h`）。
pub const CAP_SYS_ADMIN: u32 = 21;
pub const CAP_PERFMON: u32 = 38;
pub const CAP_BPF: u32 = 39;

/// 一次前置校验的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReport {
    pub kernel_release: String,
    pub kernel_ok: bool,
    pub btf_ok: bool,
    pub capability_ok: bool,
    /// 全部通过时为 `None`。
    pub reason: Option<String>,
    /// 失败时给运维的建议动作。
    pub advice: Option<String>,
}

impl PreflightReport {
    #[must_use]
    pub fn ok(&self) -> bool {
        self.kernel_ok && self.btf_ok && self.capability_ok
    }
}

/// 校验输入：真实环境读系统文件，测试注入固定内容。
#[derive(Debug, Clone)]
pub struct PreflightEnv {
    pub kernel_release: String,
    pub btf_path: PathBuf,
    pub cap_eff_path: PathBuf,
    pub euid: u32,
}

impl PreflightEnv {
    /// 读取真实环境（`uname` 由调用方通过 `/proc/sys/kernel/osrelease` 提供，
    /// 避免为一次读取引入 libc 依赖）。
    #[must_use]
    pub fn detect() -> Self {
        Self {
            kernel_release: std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
            btf_path: PathBuf::from("/sys/kernel/btf/vmlinux"),
            cap_eff_path: PathBuf::from("/proc/self/status"),
            euid: current_euid(),
        }
    }

    /// 执行校验；任一检查失败都会给出原因与建议。
    #[must_use]
    pub fn check(&self) -> PreflightReport {
        let kernel_ok = kernel_supported(&self.kernel_release);
        let btf_ok = Path::new(&self.btf_path).is_file();
        let cap_eff = std::fs::read_to_string(&self.cap_eff_path)
            .ok()
            .and_then(|text| caps_from_status(&text));
        let capability_ok = capability_ok(self.euid, cap_eff);

        let mut failures: Vec<&str> = Vec::new();
        if !kernel_ok {
            failures.push("kernel >= 5.8");
        }
        if !btf_ok {
            failures.push("/sys/kernel/btf/vmlinux");
        }
        if !capability_ok {
            failures.push("root or CAP_BPF+CAP_PERFMON");
        }

        let reason = if failures.is_empty() {
            None
        } else {
            Some(format!(
                "eBPF preflight failed: {} (kernel_release={}, euid={})",
                failures.join(", "),
                self.kernel_release,
                self.euid
            ))
        };
        let advice = if failures.is_empty() {
            None
        } else {
            Some(
                "run the agent as root, or grant CAP_BPF+CAP_PERFMON (apply the collector with sudo)"
                    .to_string(),
            )
        };
        PreflightReport {
            kernel_release: self.kernel_release.clone(),
            kernel_ok,
            btf_ok,
            capability_ok,
            reason,
            advice,
        }
    }
}

/// `release` 形如 `5.15.0-91-generic` / `6.1.0` / `4.19.90`；不可解析时按不支持处理。
#[must_use]
pub fn kernel_supported(release: &str) -> bool {
    let Some((major, minor)) = parse_kernel(release) else {
        return false;
    };
    (major, minor) >= (MIN_KERNEL_MAJOR, MIN_KERNEL_MINOR)
}

/// 解析主次版本号。
#[must_use]
pub fn parse_kernel(release: &str) -> Option<(u32, u32)> {
    let mut parts = release.trim().split(['.', '-']);
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// 从 `/proc/self/status` 文本取 `CapEff`（十六进制）。
#[must_use]
pub fn caps_from_status(status: &str) -> Option<u64> {
    for line in status.lines() {
        let Some(rest) = line.strip_prefix("CapEff:") else {
            continue;
        };
        let hex = rest.trim();
        return u64::from_str_radix(hex, 16).ok();
    }
    None
}

/// 具备 eBPF 权限：root（euid 0）或 `CAP_BPF`（配合 `CAP_PERFMON`）；`CAP_SYS_ADMIN`
/// 视作兼容旧内核的等价能力。
#[must_use]
pub fn capability_ok(euid: u32, cap_eff: Option<u64>) -> bool {
    if euid == 0 {
        return true;
    }
    let Some(caps) = cap_eff else {
        return false;
    };
    let bit = |cap: u32| caps & (1u64 << cap) != 0;
    if bit(CAP_SYS_ADMIN) {
        return true;
    }
    bit(CAP_BPF) && bit(CAP_PERFMON)
}

#[cfg(unix)]
fn current_euid() -> u32 {
    // SAFETY: `geteuid` 无参数、无副作用且线程安全。
    unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn current_euid() -> u32 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_version_parsing() {
        assert!(kernel_supported("5.8.0"));
        assert!(kernel_supported("5.15.0-91-generic"));
        assert!(kernel_supported("6.1.0-13-amd64"));
        assert!(!kernel_supported("5.4.0-150-generic"), "5.4 低于基线");
        assert!(!kernel_supported("4.19.90"), "4.19 低于基线");
        assert!(!kernel_supported(""), "空值按不支持");
        assert!(!kernel_supported("garbage"));
        assert_eq!(parse_kernel("5.10.1-x"), Some((5, 10)));
        assert_eq!(parse_kernel("x.y"), None);
    }

    #[test]
    fn cap_eff_parsing_and_capability_rules() {
        let status = "Name:\tcat\nCapEff:\t000001ffffffffff\nUid:\t1000\t1000\n";
        assert_eq!(caps_from_status(status), Some(0x0000_01ff_ffff_ffff));
        assert_eq!(caps_from_status("Name:\tcat\n"), None);
        assert_eq!(caps_from_status("CapEff:\tzzz\n"), None);

        // root 直接通过
        assert!(capability_ok(0, None));
        // CAP_BPF(39) + CAP_PERFMON(38)
        let bpf_and_perfmon = (1u64 << CAP_BPF) | (1u64 << CAP_PERFMON);
        assert!(capability_ok(1000, Some(bpf_and_perfmon)));
        // 只有 CAP_BPF（无 CAP_PERFMON）不够
        assert!(!capability_ok(1000, Some(1u64 << CAP_BPF)));
        // CAP_SYS_ADMIN 视为等价
        assert!(capability_ok(1000, Some(1u64 << CAP_SYS_ADMIN)));
        // 无能力
        assert!(!capability_ok(1000, Some(0)));
        assert!(!capability_ok(1000, None));
    }

    #[test]
    fn check_reports_failures_with_advice() {
        let dir = tempfile_stub_dir();
        let report = PreflightEnv {
            kernel_release: "5.4.0-150-generic".to_string(),
            btf_path: dir.join("missing-vmlinux"),
            cap_eff_path: dir.join("status"),
            euid: 1000,
        }
        .check();
        assert!(!report.ok());
        let reason = report.reason.clone().unwrap();
        assert!(reason.contains("kernel >= 5.8"), "{reason}");
        assert!(reason.contains("/sys/kernel/btf/vmlinux"), "{reason}");
        assert!(reason.contains("CAP_BPF"), "{reason}");
        assert!(report.advice.unwrap().contains("CAP_BPF"));

        // 全部满足的情况：构造 BTF 文件与含 CAP_BPF+CAP_PERFMON 的 status。
        let btf = dir.join("vmlinux");
        std::fs::write(&btf, b"").unwrap();
        let status = dir.join("status");
        let caps = (1u64 << CAP_BPF) | (1u64 << CAP_PERFMON);
        std::fs::write(&status, format!("CapEff:\t{caps:016x}\n")).unwrap();
        let report = PreflightEnv {
            kernel_release: "6.1.0-13-amd64".to_string(),
            btf_path: btf,
            cap_eff_path: status,
            euid: 1000,
        }
        .check();
        assert!(report.ok(), "{report:?}");
        assert_eq!(report.reason, None);
        assert_eq!(report.advice, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 测试用临时目录（不引入 tempfile 依赖）。
    fn tempfile_stub_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gse-ebpf-preflight-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

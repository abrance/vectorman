//! cgroup → 容器/Pod 反查（需求 11.1：Agent 填 `src_container_id`/`src_pod`）。
//!
//! ## 为什么按 pid 反查，而不是按 `cgroup_id`
//!
//! 内核态给的 `cgroup_id` 是 cgroup 目录的 inode。要把它映射成路径，得遍历
//! `/sys/fs/cgroup` 并 `stat` 每个目录（v1 还要区分控制器层级，本机就是 v1/hybrid），
//! 成本高且脆弱。而连接键与进程键里**已经带了 `pid`**，直接从 `/proc/<pid>/cgroup`
//! 读路径即可 —— 同一个 pid 一生只查一次并缓存，代价可忽略。
//!
//! 代价与边界：
//! - 进程已退出时 `/proc/<pid>` 不存在 → 拿不到容器信息（边缘事件可能缺），按空处理；
//! - Pod **名**需要 k8s 侧的数据（`pod<uid>` 只给出 uid），因此这里只解出 `pod_uid`，
//!   Pod 名由 `/v1/apm/services` 侧的端点表或后续 k8s 采集补齐。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// 容器的进程上下文（与内核态 `ProcessContext` 对齐 + Pod uid）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContainerInfo {
    /// 容器 ID（64 位 hex；短 ID 也按原样保留）。
    pub container_id: String,
    /// Pod uid（k8s 形态下从 `pod<uid>` 解出，其他形态为空）。
    pub pod_uid: String,
    /// 进程名（读 `/proc/<pid>/comm`，用于 `process_include/exclude` 过滤）。
    pub process_name: String,
}

impl ContainerInfo {
    /// 是否什么都没解出来（空结果不入缓存，避免把「进程刚退出」记成永久未知）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.container_id.is_empty() && self.pod_uid.is_empty() && self.process_name.is_empty()
    }
}

/// 从 cgroup 路径里解容器 ID。
///
/// 覆盖实测与文档里的常见形态（k8s + containerd/cri-o/docker、systemd slice、docker v1）：
/// - k8s cgroup v2：`/kubepods.slice/…-pod<uid>.slice/cri-containerd-<cid>.scope`
/// - k8s cgroup v1：`/kubepods/burstable/pod<uid>/<cid>`
/// - docker：`/docker/<cid>`、`docker-<cid>.scope`、`/system.slice/docker-<cid>.scope`
/// - 其他：路径里出现 64 位 hex 段（`docker`/cri 的命名都有这个特征）
#[must_use]
pub fn parse_container_id(path: &str) -> Option<String> {
    // 先按命名前缀找，再退化成「路径中任一段是 64 位 hex」。
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        let candidate = segment
            .strip_suffix(".scope")
            .unwrap_or(segment)
            .rsplit_once('-')
            .map_or(segment, |(_, tail)| tail)
            .trim_start_matches('.');
        let candidate = if is_container_id(candidate) {
            candidate
        } else {
            // 处理 `cri-containerd-<cid>.scope` 这类：上面的 rsplit_once 已取到尾部，
            // 但如果段内有多个连字符（例如 `kubepods-burstable-pod<uid>.slice`），
            // 就退回到整段判断。
            segment.strip_suffix(".scope").unwrap_or(segment)
        };
        if is_container_id(candidate) {
            return Some(candidate.to_string());
        }
        // 兜底：段里可能带括号修饰（systemd 会把路径写成 `docker-<cid>.scope` 已覆盖），
        // 以及 v1 形态下路径直接就是容器 ID 的情况（上面 `is_container_id(candidate)` 已处理）。
    }
    None
}

/// 从 cgroup 路径里解 Pod uid（k8s 形态下的 `pod<uid>`）。
///
/// 段形态不止一种：cgroup v2 是 `kubepods-burstable-pod<uid>.slice`，
/// v1 是 `pod<uid>`，也有 `pod-<uid>`。统一取**段内最后一次 `pod` 之后**的片段再去掉后缀，
/// 这样不必为每种前缀写一条分支。
#[must_use]
pub fn parse_pod_uid(path: &str) -> Option<String> {
    for segment in path.split('/').filter(|s| !s.is_empty()) {
        let Some((_, tail)) = segment.rsplit_once("pod") else {
            continue;
        };
        let mut uid = tail.trim_start_matches('-');
        for suffix in [".slice", ".scope", ".service"] {
            if let Some((head, _)) = uid.split_once(suffix) {
                uid = head;
            }
        }
        let uid = uid.trim_matches('-');
        if is_pod_uid(uid) {
            return Some(uid.to_string());
        }
    }
    None
}

/// 64 位 hex（大小写都认）。
#[must_use]
pub fn is_container_id(value: &str) -> bool {
    value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// Pod uid：带连字符的 uuid（36）或纯 hex（32）。
fn is_pod_uid(value: &str) -> bool {
    let stripped: String = value.chars().filter(|c| *c != '-').collect();
    matches!(value.len(), 36 | 32)
        && stripped.len() >= 32
        && stripped.chars().all(|c| c.is_ascii_hexdigit())
}

/// `/proc/<pid>/cgroup` 的解析（取 `0::` 那行；v1 下取任意一行并合并判断）。
///
/// k8s cgroup v1 的容器信息在某个控制器的路径里（通常是 `pids` 或 `systemd` 那行），
/// 所以这里把**所有行**都试一遍，而不是只看 v2 的 `0::`。
#[must_use]
pub fn parse_proc_cgroup(content: &str) -> ContainerInfo {
    let mut info = ContainerInfo::default();
    for line in content.lines() {
        let Some((_, path)) = line.split_once(':') else {
            continue;
        };
        let Some((_, path)) = path.split_once(':') else {
            continue;
        };
        if info.container_id.is_empty() {
            if let Some(id) = parse_container_id(path) {
                info.container_id = id;
            }
        }
        if info.pod_uid.is_empty() {
            if let Some(uid) = parse_pod_uid(path) {
                info.pod_uid = uid;
            }
        }
    }
    info
}

/// pid → 容器信息的缓存。
///
/// `ttl` 内命中直接返回；未命中读一次 `/proc/<pid>/cgroup` 与 `/proc/<pid>/comm`。
/// 进程退出后 `/proc/<pid>` 消失，此时返回 `None`（**不缓存空结果**：同一个 pid 可能被复用，
/// 把「未知」记成长期事实会污染后续反查）。
pub struct ProcessResolver {
    ttl: Duration,
    max_entries: usize,
    cache: HashMap<u32, (Instant, ContainerInfo)>,
    proc_root: PathBuf,
}

impl ProcessResolver {
    #[must_use]
    pub fn new(ttl_secs: u64, max_entries: usize) -> Self {
        Self {
            ttl: Duration::from_secs(ttl_secs.max(1)),
            max_entries: max_entries.max(1),
            cache: HashMap::new(),
            proc_root: PathBuf::from("/proc"),
        }
    }

    /// 测试用：替换 `/proc` 根目录。
    #[must_use]
    pub fn with_proc_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.proc_root = root.into();
        self
    }

    /// 反查一个 pid；进程已退出或读不到时返回 `None`。
    pub fn resolve(&mut self, pid: u32) -> Option<ContainerInfo> {
        if pid == 0 {
            return None;
        }
        if let Some((at, info)) = self.cache.get(&pid) {
            if at.elapsed() < self.ttl {
                return Some(info.clone());
            }
        }
        let info = self.read_proc(pid)?;
        if info.is_empty() {
            return None;
        }
        // 简单容量保护：满了就整体清空（条目数远小于进程数，不做 LRU）。
        if self.cache.len() >= self.max_entries {
            self.cache.clear();
        }
        self.cache.insert(pid, (Instant::now(), info.clone()));
        Some(info)
    }

    /// 当前缓存条目数（自监控）。
    #[must_use]
    pub fn cached(&self) -> usize {
        self.cache.len()
    }

    fn read_proc(&self, pid: u32) -> Option<ContainerInfo> {
        let content =
            std::fs::read_to_string(self.proc_root.join(pid.to_string()).join("cgroup")).ok()?;
        let mut info = parse_proc_cgroup(&content);
        let comm = std::fs::read_to_string(self.proc_root.join(pid.to_string()).join("comm"))
            .ok()
            .map(|c| c.trim().to_string())
            .unwrap_or_default();
        info.process_name = comm;
        Some(info)
    }
}

/// 便于日志/自监控的展示。
#[must_use]
pub fn describe(info: &ContainerInfo) -> String {
    match (info.container_id.as_str(), info.pod_uid.as_str()) {
        ("", "") => "host".to_string(),
        (id, "") => format!("container:{}", short(id)),
        ("", uid) => format!("pod:{}", short(uid)),
        (id, uid) => format!("pod:{} container:{}", short(uid), short(id)),
    }
}

/// 取前 12 位（docker 短 ID 习惯），便于日志可读。
#[must_use]
pub fn short(value: &str) -> String {
    value.chars().take(12).collect()
}

/// 把 `/proc/<pid>/cgroup` 的文件路径拆出来（测试用）。
#[must_use]
pub fn cgroup_path(proc_root: &Path, pid: u32) -> PathBuf {
    proc_root.join(pid.to_string()).join("cgroup")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_k8s_cgroup_v2_paths() {
        let path = "/kubepods.slice/kubepods-burstable.slice/kubepods-burstable-pod9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210.slice/cri-containerd-6a3f2b1c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a.scope";
        // 路径里的 `pod<uid>` 段与容器段各取所需。
        assert_eq!(
            parse_container_id(path).as_deref(),
            Some("6a3f2b1c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a")
        );
        assert_eq!(
            parse_pod_uid(path).as_deref(),
            Some("9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210")
        );
    }

    #[test]
    fn parses_k8s_cgroup_v1_and_docker_paths() {
        let v1 = "11:pids:/kubepods/burstable/pod9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210/\
                  6a3f2b1c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a";
        assert_eq!(
            parse_container_id(v1).as_deref(),
            Some("6a3f2b1c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a")
        );
        assert_eq!(
            parse_pod_uid(v1).as_deref(),
            Some("9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210")
        );

        let docker = "/docker/6a3f2b1c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a";
        assert!(parse_container_id(docker).is_some());
        let systemd = "/system.slice/docker-6a3f2b1c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a.scope";
        assert_eq!(
            parse_container_id(systemd).as_deref(),
            Some("6a3f2b1c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a")
        );
    }

    #[test]
    fn host_paths_have_no_container() {
        // 本机实测：普通 systemd 用户会话（没有容器）
        let host = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/pi-web.service";
        assert_eq!(parse_container_id(host), None);
        assert_eq!(parse_pod_uid(host), None);
        assert_eq!(parse_container_id("/"), None);
        assert_eq!(parse_container_id(""), None);
        // 短 ID 不是容器 ID（避免把 `app-1234.service` 这类误判）
        assert_eq!(parse_container_id("/system.slice/app-1234.service"), None);
    }

    #[test]
    fn parses_proc_cgroup_merging_all_lines() {
        // v1：容器段出现在 pids 行，v2 行是宿主机路径 —— 必须把所有行都试一遍。
        let content = "11:pids:/kubepods/burstable/pod9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210/\
                       6a3f2b1c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a\n\
                       1:name=systemd:/kubepods/burstable/pod9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210\n\
                       0::/kubepods.slice/kubepods-burstable.slice\n";
        let info = parse_proc_cgroup(content);
        assert!(is_container_id(&info.container_id));
        assert_eq!(
            info.pod_uid,
            "9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210".to_string()
        );

        let host = parse_proc_cgroup("0::/user.slice/user-1000.slice/user@1000.service\n");
        assert!(host.is_empty());
    }

    #[test]
    fn resolver_reads_proc_and_caches() {
        let dir = tempfile::tempdir().unwrap();
        let pid_dir = dir.path().join("4242");
        std::fs::create_dir_all(&pid_dir).unwrap();
        std::fs::write(
            pid_dir.join("cgroup"),
            "0::/kubepods.slice/kubepods-burstable.slice/\
             kubepods-burstable-pod9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210.slice/\
             cri-containerd-6a3f2b1c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a.scope\n",
        )
        .unwrap();
        std::fs::write(pid_dir.join("comm"), "java\n").unwrap();

        let mut resolver = ProcessResolver::new(60, 128).with_proc_root(dir.path());
        let info = resolver.resolve(4242).expect("应解出容器信息");
        assert_eq!(info.process_name, "java");
        assert!(is_container_id(&info.container_id));
        assert_eq!(info.pod_uid, "9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210");
        assert_eq!(resolver.cached(), 1);

        // 进程不存在 → None，且**不缓存空结果**（pid 会被复用）。
        assert!(resolver.resolve(9999).is_none());
        assert_eq!(resolver.cached(), 1);
        // pid 0 直接跳过。
        assert!(resolver.resolve(0).is_none());

        // 缓存命中：删掉 `/proc/<pid>` 后仍能拿到（TTL 内）。
        std::fs::remove_dir_all(&pid_dir).unwrap();
        assert!(resolver.resolve(4242).is_some(), "TTL 内走缓存");
    }

    #[test]
    fn describe_and_short_are_readable() {
        let info = ContainerInfo {
            container_id: "6a3f2b1c9d8e7f6a".repeat(4),
            pod_uid: "9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210".into(),
            process_name: "java".into(),
        };
        let text = describe(&info);
        assert!(text.starts_with("pod:9f8e7d6c-5b4"), "{text}");
        assert!(text.contains("container:6a3f2b1c9d8e"), "{text}");
        assert_eq!(describe(&ContainerInfo::default()), "host");
        assert_eq!(short("6a3f2b1c9d8e7f6a"), "6a3f2b1c9d8e");
    }
}

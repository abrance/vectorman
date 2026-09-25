//! cgroup → 容器/Pod 反查（需求 11.1：Agent 填 `src_container_id`/`src_pod`）。
//!
//! ## 为什么按 pid 反查，而不是按 `cgroup_id`
//!
//! 内核态给的 `cgroup_id` 是 cgroup 目录的 inode。要把它映射成路径，得遍历
//! `/sys/fs/cgroup` 并 `stat` 每个目录（v1 还要区分控制器层级，本机就是 v1/hybrid），
//! 成本高且脆弱。而连接键与进程键里**已经带了 `pid`**，直接从 `/proc/<pid>/cgroup`
//! 读路径即可 —— 同一个 pid 一生只查一次并缓存，代价可忽略。
//!
//! - Pod **名**：`/proc/<pid>/cgroup` 只能给出 Pod **uid**，名称要通过 Kubernetes API 反查
//!   （`uid → name`）。因此本模块支持注入一个**索引加载器**（由调用方提供，见
//!   [`ProcessResolver::with_pod_names`]）：只在真正遇到 Pod uid 时按 TTL 拉一次全量索引，
//!   k8s 不可用时退回 uid（`src_pod` 语义与加索引前一致，不会有回归）。

use std::sync::Arc;

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
    /// Pod 名（k8s API 反查得到；未启用或查不到时为空）。
    pub pod_name: String,
    /// 进程名（读 `/proc/<pid>/comm`，用于 `process_include/exclude` 过滤）。
    pub process_name: String,
}

impl ContainerInfo {
    /// 是否什么都没解出来（空结果不入缓存，避免把「进程刚退出」记成永久未知）。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.container_id.is_empty() && self.pod_uid.is_empty() && self.process_name.is_empty()
    }

    /// `src_pod` 用哪个值：优先 Pod **名**（dataserver 的端点表是按名字匹配的），
    /// 反查不到时退回 uid —— 退回时与「没有 k8s 索引」的行为完全一致，不是回归。
    #[must_use]
    pub fn pod_label(&self) -> String {
        if self.pod_name.is_empty() {
            self.pod_uid.clone()
        } else {
            self.pod_name.clone()
        }
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
    /// `uid → Pod 名` 的全量索引加载器（由调用方注入；`None` 表示不做名称反查）。
    pod_names: Option<PodNameLoader>,
    /// 已加载的索引与加载时刻（按 [`POD_INDEX_TTL`] 过期）。
    pod_index: HashMap<String, String>,
    pod_index_at: Option<Instant>,
}

/// Pod 名索引加载器：一次全量 `uid → name`（失败返回 Err，调用方退回 uid）。
pub type PodNameLoader = Arc<dyn Fn() -> Result<HashMap<String, String>, String> + Send + Sync>;

/// 索引 TTL。Pod 名几乎不变，拉一次能用很久；太短是白白打 apiserver。
const POD_INDEX_TTL: Duration = Duration::from_secs(300);

impl ProcessResolver {
    #[must_use]
    pub fn new(ttl_secs: u64, max_entries: usize) -> Self {
        Self {
            ttl: Duration::from_secs(ttl_secs.max(1)),
            max_entries: max_entries.max(1),
            cache: HashMap::new(),
            proc_root: PathBuf::from("/proc"),
            pod_names: None,
            pod_index: HashMap::new(),
            pod_index_at: None,
        }
    }

    /// 注入 Pod 名索引加载器（`uid → name`）。
    ///
    /// **只在必要时调用**：解析过程中遇到 Pod uid 且索引过期时才加载一次，
    /// 宿主机进程（无 Pod uid）与没有 k8s 的机器都不会触发。
    #[must_use]
    pub fn with_pod_names(mut self, loader: PodNameLoader) -> Self {
        self.pod_names = Some(loader);
        self
    }

    /// 同 [`ProcessResolver::with_pod_names`]，但接受 `Option`（调用方直接透传配置）。
    #[must_use]
    pub fn with_pod_names_opt(mut self, loader: Option<PodNameLoader>) -> Self {
        self.pod_names = loader;
        self
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
        let mut info = self.read_proc(pid)?;
        self.fill_pod_name(&mut info);
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

    /// 当前已加载的 Pod 名索引条目数（自监控）。
    #[must_use]
    pub fn pod_index_len(&self) -> usize {
        self.pod_index.len()
    }

    /// 用 k8s 索引补 Pod 名。
    ///
    /// 索引按 TTL 失效；加载失败**不报错也不清空旧索引**（旧名字比没有名字有用），
    /// 只是把加载时刻推后，避免每个 pid 都去打一次 apiserver。
    fn fill_pod_name(&mut self, info: &mut ContainerInfo) {
        if info.pod_uid.is_empty() {
            return;
        }
        let Some(loader) = self.pod_names.clone() else {
            return;
        };
        let stale = self
            .pod_index_at
            .is_none_or(|at| at.elapsed() >= POD_INDEX_TTL);
        if stale {
            match loader() {
                Ok(index) => {
                    self.pod_index = index;
                    self.pod_index_at = Some(Instant::now());
                }
                Err(reason) => {
                    eprintln!("gse-agent: Pod 名索引加载失败，退回 uid：{reason}");
                    self.pod_index_at = Some(Instant::now());
                }
            }
        }
        if let Some(name) = self.pod_index.get(&info.pod_uid) {
            info.pod_name = name.clone();
        }
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

/// 便于日志/自监控的展示（Pod 优先显示名字，名字未知才显示 uid 前缀）。
#[must_use]
pub fn describe(info: &ContainerInfo) -> String {
    let pod = if !info.pod_name.is_empty() {
        info.pod_name.clone()
    } else {
        short(&info.pod_uid)
    };
    match (info.container_id.as_str(), pod.as_str()) {
        ("", "") => "host".to_string(),
        (id, "") => format!("container:{}", short(id)),
        ("", _) => format!("pod:{pod}"),
        (id, _) => format!("pod:{pod} container:{}", short(id)),
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
        let mut info = ContainerInfo {
            container_id: "6a3f2b1c9d8e7f6a".repeat(4),
            pod_uid: "9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210".into(),
            pod_name: String::new(),
            process_name: "java".into(),
        };
        let text = describe(&info);
        assert!(text.starts_with("pod:9f8e7d6c-5b4"), "{text}");
        assert!(text.contains("container:6a3f2b1c9d8e"), "{text}");
        assert_eq!(describe(&ContainerInfo::default()), "host");
        assert_eq!(short("6a3f2b1c9d8e7f6a"), "6a3f2b1c9d8e");

        // 有 Pod 名时展示名字；`pod_label()` 也优先用名字。
        info.pod_name = "order-api-7c9f".into();
        assert_eq!(describe(&info), "pod:order-api-7c9f container:6a3f2b1c9d8e");
        assert_eq!(info.pod_label(), "order-api-7c9f");
        info.pod_name.clear();
        assert_eq!(info.pod_label(), "9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210");
    }

    /// Pod 名反查：只在遇到 Pod uid 时按 TTL 拉一次索引；失败退回 uid。
    #[test]
    fn resolver_fills_pod_name_from_injected_index() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        const CGROUP: &str = "0::/kubepods.slice/kubepods-burstable.slice/\
            kubepods-burstable-pod9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210.slice/\
            cri-containerd-6a3f2b1c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a.scope\n";
        const UID: &str = "9f8e7d6c-5b4a-3210-9f8e-7d6c5b4a3210";

        let dir = tempfile::tempdir().unwrap();
        for pid in [4242, 4243] {
            let pid_dir = dir.path().join(pid.to_string());
            std::fs::create_dir_all(&pid_dir).unwrap();
            std::fs::write(pid_dir.join("cgroup"), CGROUP).unwrap();
            std::fs::write(pid_dir.join("comm"), "java\n").unwrap();
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let loader_calls = calls.clone();
        let loader_calls2 = calls.clone();
        let mut resolver = ProcessResolver::new(60, 8)
            .with_proc_root(dir.path())
            .with_pod_names(Arc::new(move || {
                loader_calls.fetch_add(1, Ordering::SeqCst);
                Ok(HashMap::from([(UID.to_string(), "order-api-7c9f".into())]))
            }));

        let info = resolver.resolve(4242).expect("解析成功");
        assert_eq!(info.pod_name, "order-api-7c9f");
        assert_eq!(info.pod_label(), "order-api-7c9f");
        assert_eq!(resolver.pod_index_len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // 同一 uid 的第二个 pid：复用已加载的索引，不再打 apiserver。
        let info = resolver.resolve(4243).expect("解析成功");
        assert_eq!(info.pod_name, "order-api-7c9f");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "索引只加载一次");

        // 加载失败：不报错，退回 uid（与没有索引时行为一致）。
        let mut failing = ProcessResolver::new(60, 8)
            .with_proc_root(dir.path())
            .with_pod_names(Arc::new(|| Err("apiserver 不可达".to_string())));
        let info = failing.resolve(4242).expect("解析成功");
        assert!(info.pod_name.is_empty());
        assert_eq!(info.pod_label(), UID);

        // 未注入加载器：完全不反查（宿主机与非 k8s 环境零开销）。
        let mut plain = ProcessResolver::new(60, 8).with_proc_root(dir.path());
        let info = plain.resolve(4242).expect("解析成功");
        assert!(info.pod_name.is_empty());
        assert_eq!(info.pod_label(), UID);

        // 宿主机进程（无 Pod uid）不会触发索引加载。
        let host_dir = tempfile::tempdir().unwrap();
        let pid_dir = host_dir.path().join("77");
        std::fs::create_dir_all(&pid_dir).unwrap();
        std::fs::write(pid_dir.join("cgroup"), "0::/user.slice/user-1000.slice\n").unwrap();
        let before = calls.load(Ordering::SeqCst);
        let mut host = ProcessResolver::new(60, 8)
            .with_proc_root(host_dir.path())
            .with_pod_names(Arc::new(move || {
                loader_calls2.fetch_add(1, Ordering::SeqCst);
                Ok(HashMap::new())
            }));
        let info = host.resolve(77).expect("解析成功");
        assert!(info.pod_label().is_empty(), "宿主机进程没有 Pod 标签");
        assert_eq!(calls.load(Ordering::SeqCst), before, "宿主机不拉索引");
    }
}

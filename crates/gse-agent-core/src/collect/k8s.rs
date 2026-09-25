//! K8s Pod 标准输出采集：list Pods → 逐容器 follow 日志流 → 清洗攒批。
//!
//! 对齐 `kubectl logs`：调用 Kubernetes pod log API（非节点文件），`follow=true`。

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::sync::mpsc;

use super::clean::{detect_level, Cleaner};
use super::config::CollectorConfig;
use super::envelope::{LogsRecord, DATA_TYPE_LOGS};
use super::glob::glob_match;
use super::kubeconfig;
use super::CollectShared;

/// list Pods 得到的精简信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodSummary {
    pub name: String,
    /// `metadata.uid`（eBPF 侧的 cgroup 只给 uid，需要它反查 Pod 名）。
    pub uid: String,
    pub containers: Vec<String>,
}

/// 从 `pods` 响应解析 Pod 名与容器名。
pub fn parse_pods(v: &Value) -> Vec<PodSummary> {
    let Some(items) = v.get("items").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let name = item
                .pointer("/metadata/name")
                .and_then(|x| x.as_str())?
                .to_string();
            let uid = item
                .pointer("/metadata/uid")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            let containers = item
                .pointer("/spec/containers")
                .and_then(|x| x.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Some(PodSummary {
                name,
                uid,
                containers,
            })
        })
        .collect()
}

/// `uid → Pod 名` 索引（eBPF 的 cgroup 反查用：`/proc/<pid>/cgroup` 只给 uid）。
///
/// 没有 uid 的条目跳过：空键会把所有 Pod 混成一条。
#[must_use]
pub fn pod_name_index(pods: &[PodSummary]) -> HashMap<String, String> {
    pods.iter()
        .filter(|p| !p.uid.is_empty())
        .map(|p| (p.uid.clone(), p.name.clone()))
        .collect()
}

/// 拉取一次 Pod 名索引（供 eBPF 的 Pod 名反查使用）。
///
/// `namespace` 为空时列全部命名空间（eBPF 采集面向整机，不该被单个命名空间限制）。
/// 凭据解析失败或接口不可达都返回 `Err`，调用方退回 uid 即可（不致命）。
pub fn list_pod_name_index(cfg: &CollectorConfig) -> Result<HashMap<String, String>, String> {
    let cred = kubeconfig::resolve(&cfg.kubeconfig)?;
    let client = K8sClient::new(cred.base_url, cred.token);
    let pods = if cfg.namespace.trim().is_empty() {
        client.list_pods("")?
    } else {
        client.list_pods(cfg.namespace.trim())?
    };
    Ok(pod_name_index(&pods))
}

/// 按 Pod 名 glob 与容器配置选出要 follow 的 `(pod, container)`。
pub fn select_targets(
    pods: &[PodSummary],
    pattern: &str,
    container: &str,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for pod in pods {
        if !glob_match(pattern, &pod.name) {
            continue;
        }
        if container.is_empty() {
            for c in &pod.containers {
                out.push((pod.name.clone(), c.clone()));
            }
        } else if pod.containers.iter().any(|c| c == container) {
            out.push((pod.name.clone(), container.to_string()));
        }
    }
    out
}

/// 构造 pod log 接口路径；`tail` 且 `start_n>=1` 映射为 `tailLines`。
pub fn log_path(namespace: &str, pod: &str, container: &str, cfg: &CollectorConfig) -> String {
    let mut path =
        format!("/api/v1/namespaces/{namespace}/pods/{pod}/log?follow=true&container={container}");
    if cfg.start_mode() != "head" && cfg.start_n >= 1 {
        path.push_str(&format!("&tailLines={}", cfg.start_n));
    }
    path
}

/// apiserver 客户端；token 为空时不带 Authorization 头。
#[derive(Clone)]
pub struct K8sClient {
    agent: ureq::Agent,
    base_url: String,
    token: String,
}

impl K8sClient {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(30))
            .build();
        Self {
            agent,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
        }
    }

    fn get(&self, path: &str) -> ureq::Request {
        let url = format!("{}{}", self.base_url, path);
        let req = self.agent.get(&url);
        if self.token.is_empty() {
            req
        } else {
            req.set("Authorization", &format!("Bearer {}", self.token))
        }
    }

    /// `GET /api/v1/namespaces/{ns}/pods`；`namespace` 为空时列全部命名空间。
    pub fn list_pods(&self, namespace: &str) -> Result<Vec<PodSummary>, String> {
        let path = if namespace.trim().is_empty() {
            "/api/v1/pods".to_string()
        } else {
            format!("/api/v1/namespaces/{namespace}/pods")
        };
        let resp = self
            .get(&path)
            .call()
            .map_err(|e| format!("list pods: {e}"))?;
        let v: Value = resp.into_json().map_err(|e| format!("decode pods: {e}"))?;
        Ok(parse_pods(&v))
    }

    /// 打开一条 follow 日志流。
    pub fn open_log(
        &self,
        namespace: &str,
        pod: &str,
        container: &str,
        cfg: &CollectorConfig,
    ) -> Result<Box<dyn Read + Send + 'static>, String> {
        let path = log_path(namespace, pod, container, cfg);
        let resp = self
            .get(&path)
            .call()
            .map_err(|e| format!("open pod log {pod}/{container}: {e}"))?;
        Ok(resp.into_reader())
    }
}

/// 采集任务：每 30s 重新 list，按需要 follow 的 Pod/容器增删流，行经清洗后攒批。
pub async fn run(shared: Arc<CollectShared>, item_id: String, cfg: CollectorConfig) {
    let cred = match kubeconfig::resolve(&cfg.kubeconfig) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("gse-agent: k8s credential unavailable: {e}");
            return;
        }
    };
    let client = Arc::new(K8sClient::new(cred.base_url, cred.token));
    let cleaner = Arc::new(Cleaner::new(&cfg.clean));
    let (tx, mut rx) = mpsc::unbounded_channel::<FollowLine>();
    let mut active = Followers::default();
    let mut pending: Vec<Value> = Vec::new();
    let batch_max = cfg.batch_max_records.max(1);
    let flush = Duration::from_secs(cfg.flush_interval_secs.max(1));
    let mut list_tick = tokio::time::interval(Duration::from_secs(30));
    let mut flush_tick = tokio::time::interval(flush);

    loop {
        tokio::select! {
            _ = list_tick.tick() => {
                refresh_targets(&client, &cfg, &tx, &mut active);
            }
            _ = flush_tick.tick() => {
                while pending.len() >= batch_max {
                    let chunk: Vec<Value> = pending.drain(..batch_max).collect();
                    shared.push(DATA_TYPE_LOGS, &item_id, chunk).await;
                }
                if !pending.is_empty() {
                    let chunk = std::mem::take(&mut pending);
                    shared.push(DATA_TYPE_LOGS, &item_id, chunk).await;
                }
            }
            Some(line) = rx.recv() => {
                if let Some(cleaned) = cleaner.clean(&line.text) {
                    let rec = LogsRecord {
                        record_id: format!("{}:{}:{}", shared.agent_id, line.source, line.seq),
                        timestamp: line.ts_micros,
                        level: detect_level(&line.text),
                        message: cleaned.message,
                        source: line.source,
                        labels: cleaned.labels,
                    };
                    pending.push(serde_json::to_value(rec).unwrap_or(Value::Null));
                }
            }
        }
    }
}

/// 有 follow 线程的集合；Drop 时通知全部线程退出（supervisor abort 本任务时触发）。
#[derive(Default)]
struct Followers(HashMap<(String, String), Arc<AtomicBool>>);

impl Drop for Followers {
    fn drop(&mut self) {
        for stop in self.0.values() {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

/// 重新 list 并增删 follow 流。
fn refresh_targets(
    client: &K8sClient,
    cfg: &CollectorConfig,
    tx: &mpsc::UnboundedSender<FollowLine>,
    active: &mut Followers,
) {
    let pods = match client.list_pods(&cfg.namespace) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("gse-agent: k8s list pods failed: {e}");
            return;
        }
    };
    let desired = select_targets(&pods, &cfg.pod_name_pattern, &cfg.container);
    let desired_set: HashSet<(String, String)> = desired.iter().cloned().collect();

    // 停掉不再匹配的流。
    let stale: Vec<(String, String)> = active
        .0
        .keys()
        .filter(|k| !desired_set.contains(*k))
        .cloned()
        .collect();
    for key in stale {
        if let Some(stop) = active.0.remove(&key) {
            stop.store(true, Ordering::Relaxed);
        }
    }

    // 为新匹配的 Pod/容器开 follow。
    for (pod, container) in desired {
        let key = (pod.clone(), container.clone());
        if active.0.contains_key(&key) {
            continue;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let client = client.clone();
        let ns = cfg.namespace.clone();
        let cfg = cfg.clone();
        let tx = tx.clone();
        let stop_thread = stop.clone();
        std::thread::spawn(move || {
            follow_thread(client, ns, pod, container, cfg, tx, stop_thread);
        });
        active.0.insert(key, stop);
    }
}

/// 每条 follow 流一个线程；断线指数退避重连同一 URL。
fn follow_thread(
    client: K8sClient,
    namespace: String,
    pod: String,
    container: String,
    cfg: CollectorConfig,
    tx: mpsc::UnboundedSender<FollowLine>,
    stop: Arc<AtomicBool>,
) {
    let source = format!("{namespace}/{pod}/{container}");
    let mut backoff = 1u64;
    while !stop.load(Ordering::Relaxed) {
        match client.open_log(&namespace, &pod, &container, &cfg) {
            Ok(reader) => {
                backoff = 1;
                let mut lines = BufReader::new(reader).lines();
                loop {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    match lines.next() {
                        Some(Ok(text)) => {
                            let line = FollowLine {
                                source: source.clone(),
                                text,
                                ts_micros: super::now_micros(),
                                seq: FOLLOW_SEQ.fetch_add(1, Ordering::Relaxed),
                            };
                            if tx.send(line).is_err() {
                                return;
                            }
                        }
                        Some(Err(_)) | None => break,
                    }
                }
            }
            Err(e) => eprintln!("gse-agent: k8s follow {source} failed: {e}"),
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        std::thread::sleep(Duration::from_secs(backoff));
        backoff = (backoff * 2).min(60);
    }
}

static FOLLOW_SEQ: AtomicU64 = AtomicU64::new(0);

struct FollowLine {
    source: String,
    text: String,
    ts_micros: i64,
    seq: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    fn pod(name: &str, containers: &[&str]) -> PodSummary {
        PodSummary {
            name: name.to_string(),
            uid: String::new(),
            containers: containers.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// `uid → name` 索引：eBPF 的 cgroup 只给 uid，Pod 名要靠它反查。
    #[test]
    fn parse_pods_reads_uids_and_builds_index() {
        let v = serde_json::json!({
            "items": [
                {"metadata": {"name": "order-api-1", "uid": "uid-1"}, "spec": {"containers": [{"name": "app"}]}},
                {"metadata": {"name": "web-1", "uid": "uid-2"}, "spec": {"containers": [{"name": "app"}]}},
                // 没有 uid 的条目要跳过：空键会把所有 Pod 混成一条。
                {"metadata": {"name": "no-uid"}, "spec": {"containers": [{"name": "app"}]}}
            ]
        });
        let pods = parse_pods(&v);
        assert_eq!(pods.len(), 3);
        assert_eq!(pods[0].uid, "uid-1");
        let index = pod_name_index(&pods);
        assert_eq!(index.len(), 2);
        assert_eq!(index.get("uid-1").map(String::as_str), Some("order-api-1"));
        assert_eq!(index.get("uid-2").map(String::as_str), Some("web-1"));
        assert!(!index.contains_key(""), "无 uid 的条目不进索引");
        // 缺 metadata.uid 时字段为空而不是解析失败。
        assert_eq!(pods[2].uid, "");
    }

    /// `list_pod_name_index` 全链路：kubeconfig → apiserver → `uid → Pod 名`。
    ///
    /// 这条链是 eBPF 的 Pod 名反查的实际入口（本机没有 k8s，用假 apiserver 覆盖）。
    #[test]
    fn list_pod_name_index_end_to_end() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 2048];
                let _ = std::io::Read::read(&mut stream, &mut buf);
                let body = r#"{"items":[
                    {"metadata":{"name":"order-api-1","uid":"uid-1"},"spec":{"containers":[{"name":"app"}]}},
                    {"metadata":{"name":"web-1","uid":"uid-2"},"spec":{"containers":[{"name":"app"}]}}
                ]}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let path = std::env::temp_dir().join(format!(
            "vectorman-k8s-{}-{}.yaml",
            std::process::id(),
            addr.port()
        ));
        std::fs::write(&path, format!("server: \"http://{addr}\"\ntoken: t\n")).unwrap();
        let cfg = CollectorConfig {
            kubeconfig: path.to_string_lossy().to_string(),
            ..CollectorConfig::default()
        };
        let index = list_pod_name_index(&cfg);
        let _ = std::fs::remove_file(&path);
        let index = index.expect("索引应加载成功");
        assert_eq!(index.get("uid-1").map(String::as_str), Some("order-api-1"));
        assert_eq!(index.get("uid-2").map(String::as_str), Some("web-1"));
    }

    /// 空命名空间走集群级路径（eBPF 采集面向整机，不该被单个命名空间限制）。
    #[test]
    fn list_pods_without_namespace_uses_cluster_path() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(std::sync::Mutex::new(String::new()));
        let seen_in_thread = seen.clone();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let n = std::io::Read::read(&mut stream, &mut buf).unwrap_or(0);
                *seen_in_thread.lock().unwrap() = String::from_utf8_lossy(&buf[..n]).to_string();
                let body =
                    r#"{"items":[{"metadata":{"name":"a","uid":"u1"},"spec":{"containers":[]}}]}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        let client = K8sClient::new(format!("http://{addr}"), "");
        let pods = client.list_pods("").expect("list pods");
        assert_eq!(
            pod_name_index(&pods).get("u1").map(String::as_str),
            Some("a")
        );
        assert!(
            seen.lock().unwrap().starts_with("GET /api/v1/pods "),
            "空命名空间应请求 /api/v1/pods：{}",
            seen.lock().unwrap()
        );
    }

    #[test]
    fn parse_pods_reads_names_and_containers() {
        let v = serde_json::json!({
            "items": [
                {"metadata": {"name": "nginx-1"}, "spec": {"containers": [{"name": "nginx"}, {"name": "sidecar"}]}},
                {"metadata": {"name": "web-1"}, "spec": {"containers": [{"name": "web"}]}}
            ]
        });
        let pods = parse_pods(&v);
        assert_eq!(pods.len(), 2);
        assert_eq!(pods[0].name, "nginx-1");
        assert_eq!(pods[0].containers, vec!["nginx", "sidecar"]);
    }

    #[test]
    fn select_targets_honors_glob_and_container() {
        let pods = vec![
            pod("nginx-1", &["nginx", "sidecar"]),
            pod("nginx-2", &["nginx"]),
            pod("web-1", &["web"]),
        ];
        let all = select_targets(&pods, "nginx-*", "");
        assert_eq!(
            all,
            vec![
                ("nginx-1".to_string(), "nginx".to_string()),
                ("nginx-1".to_string(), "sidecar".to_string()),
                ("nginx-2".to_string(), "nginx".to_string()),
            ]
        );
        let one = select_targets(&pods, "nginx-*", "nginx");
        assert_eq!(
            one,
            vec![
                ("nginx-1".to_string(), "nginx".to_string()),
                ("nginx-2".to_string(), "nginx".to_string()),
            ]
        );
        // 指定容器不在 Pod 中则该 Pod 不采集。
        assert!(select_targets(&pods, "web-*", "nginx").is_empty());
    }

    #[test]
    fn log_path_maps_start_markers_to_tail_lines() {
        let mut cfg = CollectorConfig {
            namespace: "default".to_string(),
            ..Default::default()
        };
        cfg.start_mode = "tail".to_string();
        cfg.start_n = 0;
        assert!(!log_path("default", "p", "c", &cfg).contains("tailLines"));

        cfg.start_n = 50;
        assert!(log_path("default", "p", "c", &cfg).contains("tailLines=50"));

        cfg.start_mode = "head".to_string();
        let head = log_path("default", "p", "c", &cfg);
        assert!(head.contains("follow=true"));
        assert!(!head.contains("tailLines"));
    }

    /// 极简 mock apiserver：读一次请求后返回固定响应。
    fn mock_server(status: &str, content_type: &str, body: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let status = status.to_string();
        let content_type = content_type.to_string();
        let body = body.to_string();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn list_pods_via_http() {
        let base = mock_server(
            "200 OK",
            "application/json",
            r#"{"items":[{"metadata":{"name":"nginx-1"},"spec":{"containers":[{"name":"nginx"}]}}]}"#,
        );
        let client = K8sClient::new(base, "token");
        let pods = client.list_pods("default").expect("list");
        assert_eq!(pods.len(), 1);
        assert_eq!(pods[0].name, "nginx-1");
    }

    #[test]
    fn list_pods_surfaces_http_error() {
        let base = mock_server("403 Forbidden", "application/json", r#"{"message":"nope"}"#);
        let client = K8sClient::new(base, "");
        assert!(client.list_pods("default").is_err());
    }

    #[test]
    fn open_log_streams_lines() {
        let base = mock_server("200 OK", "text/plain", "line1\nline2\n");
        let client = K8sClient::new(base, "");
        let cfg = CollectorConfig::default();
        let reader = client.open_log("default", "p", "c", &cfg).expect("open");
        let lines: Vec<String> = BufReader::new(reader).lines().map(|l| l.unwrap()).collect();
        assert_eq!(lines, vec!["line1", "line2"]);
    }
}

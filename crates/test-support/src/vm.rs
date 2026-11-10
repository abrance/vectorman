//! VictoriaMetrics 直查：证明数据**真的落在 VM 里**，而不是 dataserver 的缓存/内存态。
//!
//! 地址与 `scripts/test-integration.sh` 的 `DP_E2E_VM_*` 约定一致：
//! - `DP_E2E_VM_INSERT_URL`：必填，vminsert 或单机版基址。
//! - `DP_E2E_VM_SELECT_URL`：缺省回落为 insert 地址。
//! - `DP_E2E_VM_SINGLE_NODE`：设了按单机版解释（路径不带 `/select/{tenant}`）。
//!
//! 查询路径必须与 `dataplane-adapter-victoriametrics` 的 `select_path()` 保持一致：
//! 单机版 `{base}/prometheus/api/v1/query`，集群版 `{base}/select/{tenant}/prometheus/api/v1/query`。

use std::time::Duration;

use crate::prom::{self, Response};
use crate::util::wait_until;

#[derive(Debug, Clone)]
pub struct Vm {
    pub insert_url: String,
    pub select_url: String,
    pub single_node: bool,
    pub tenant: String,
}

impl Vm {
    /// 从 `DP_E2E_VM_*` 读；缺必填变量时返回 `None`（调用方决定是 panic 还是跳过）。
    pub fn from_env() -> Option<Self> {
        let insert_url = std::env::var("DP_E2E_VM_INSERT_URL").unwrap_or_default();
        if insert_url.trim().is_empty() {
            return None;
        }
        let select_url = std::env::var("DP_E2E_VM_SELECT_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| insert_url.clone());
        Some(Self {
            insert_url,
            select_url,
            single_node: std::env::var("DP_E2E_VM_SINGLE_NODE")
                .ok()
                .is_some_and(|v| !v.trim().is_empty()),
            tenant: std::env::var("DP_E2E_VM_TENANT")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| "0".to_string()),
        })
    }

    /// 缺变量直接 panic 并点名：这类用例没有 VM 就无法断言，静默跳过等于假绿。
    pub fn require() -> Self {
        Self::from_env().unwrap_or_else(|| {
            panic!(
                "缺少环境变量 DP_E2E_VM_INSERT_URL：本用例要求真实 VictoriaMetrics，不能静默跳过。\
                 本地先 `docker run -d -p 8428:8428 victoriametrics/victoria-metrics` 再导出 \
                 `DP_E2E_VM_INSERT_URL=http://127.0.0.1:8428`、`DP_E2E_VM_SINGLE_NODE=1`。"
            )
        })
    }

    pub fn query_at(&self, promql: &str, time_secs: Option<f64>) -> Result<Response, String> {
        prom::query_at(&self.select_base(), promql, time_secs)
    }

    /// `prom::query_at` 自己会拼 `/api/v1/query`；这里只补 VM 的路径前缀：
    /// 单机版 `{base}/prometheus`，集群版 `{base}/select/{tenant}/prometheus`。
    fn select_base(&self) -> String {
        let base = self.select_url.trim_end_matches('/');
        if self.single_node {
            format!("{base}/prometheus")
        } else {
            format!("{base}/select/{}/prometheus", self.tenant)
        }
    }

    pub fn query(&self, promql: &str) -> Result<Response, String> {
        self.query_at(promql, None)
    }

    pub fn must_query(&self, promql: &str, time_secs: Option<f64>) -> Response {
        self.query_at(promql, time_secs)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    /// 等到 VM 能看到满足断言的数据；超时会带上 VM 上的原始响应。
    pub fn wait_query(
        &self,
        promql: &str,
        time_secs: Option<f64>,
        timeout: Duration,
        pred: impl Fn(&Response) -> bool,
    ) -> Response {
        wait_until(&format!("VM 查询 {promql} 满足断言"), timeout, || {
            let resp = self
                .query_at(promql, time_secs)
                .unwrap_or_else(|e| panic!("{e}"));
            pred(&resp).then_some(resp)
        })
    }

    /// VM 就绪探针（`/health`）；不可达时给出明确失败而不是后面查空。
    pub fn wait_ready(&self, timeout: Duration) {
        let url = format!("{}/health", self.select_url.trim_end_matches('/'));
        wait_until("VictoriaMetrics /health 就绪", timeout, || {
            matches!(crate::http::get(&url), Ok((200, _))).then_some(())
        });
    }
}

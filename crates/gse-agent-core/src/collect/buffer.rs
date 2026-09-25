//! 内存批次缓冲：确认后丢弃，失败重试队头，满则丢最旧。
//!
//! v1 不做 WAL，进程重启即清空；容量按所有批次 `records.len()` 之和统计。

use std::collections::VecDeque;

use tokio::sync::Mutex;

use super::envelope::DataEnvelope;

/// 默认缓冲条数上限。
pub const DEFAULT_MAX_RECORDS: usize = 1000;

/// 线程安全的批次缓冲。
pub struct Buffer {
    inner: Mutex<Inner>,
    max_records: usize,
}

struct Inner {
    queue: VecDeque<DataEnvelope>,
    total_records: usize,
}

impl Buffer {
    pub fn new(max_records: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                queue: VecDeque::new(),
                total_records: 0,
            }),
            max_records: max_records.max(1),
        }
    }

    /// 入队；为容纳新批将弹出最旧批次，直到总和不超过上限。
    /// 入队一条批次。
    ///
    /// 返回**被淘汰的记录数**：容量满时按「淘汰最旧」处理，调用方应当把它计入自监控 ——
    /// 这条路径上的丢数据此前只在日志里出现，Prom 上完全看不见（实测一次 60 秒的
    /// eBPF 采集丢了 2279 条边记录、只入库 149 条，而所有指标看起来都「正常」）。
    pub async fn push(&self, env: DataEnvelope) -> u64 {
        let mut dropped_total = 0u64;
        let mut guard = self.inner.lock().await;
        while !guard.queue.is_empty() && guard.total_records + env.record_count() > self.max_records
        {
            if let Some(dropped) = guard.queue.pop_front() {
                let records = dropped.record_count() as u64;
                guard.total_records -= dropped.record_count();
                dropped_total += records;
                eprintln!(
                    "gse-agent: drop oldest batch data_type={} records={}",
                    dropped.data_type, records
                );
            }
        }
        guard.total_records += env.record_count();
        guard.queue.push_back(env);
        dropped_total
    }

    /// 弹出队头；空返回 None。
    pub async fn pop_front(&self) -> Option<DataEnvelope> {
        let mut guard = self.inner.lock().await;
        let env = guard.queue.pop_front();
        if let Some(e) = &env {
            guard.total_records -= e.record_count();
        }
        env
    }

    /// 把批次放回队头（重试同一批）。
    pub async fn push_front(&self, env: DataEnvelope) {
        let mut guard = self.inner.lock().await;
        guard.total_records += env.record_count();
        guard.queue.push_front(env);
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.queue.is_empty()
    }

    #[cfg(test)]
    pub async fn len(&self) -> usize {
        self.inner.lock().await.queue.len()
    }

    #[cfg(test)]
    pub async fn total_records(&self) -> usize {
        self.inner.lock().await.total_records
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(data_type: &str, records: usize) -> DataEnvelope {
        DataEnvelope {
            batch_id: format!("b-{data_type}-{records}"),
            data_type: data_type.to_string(),
            data_id: "item".to_string(),
            agent_id: "a".to_string(),
            host_id: String::new(),
            sent_at_micros: 0,
            records: (0..records).map(|i| serde_json::json!({"i": i})).collect(),
        }
    }

    #[tokio::test]
    async fn push_pop_roundtrip_tracks_total() {
        let buf = Buffer::new(100);
        buf.push(env("logs", 3)).await;
        buf.push(env("metrics", 2)).await;
        assert_eq!(buf.len().await, 2);
        assert_eq!(buf.total_records().await, 5);

        let first = buf.pop_front().await.expect("first");
        assert_eq!(first.data_type, "logs");
        assert_eq!(buf.total_records().await, 2);
        assert!(buf.pop_front().await.is_some());
        assert!(buf.pop_front().await.is_none());
        assert!(buf.is_empty().await);
    }

    #[tokio::test]
    async fn overflow_drops_oldest_until_fits() {
        let buf = Buffer::new(5);
        buf.push(env("logs", 3)).await;
        buf.push(env("logs", 3)).await; // 3 + 3 > 5 -> 丢最旧，剩 3
        assert_eq!(buf.total_records().await, 3);
        buf.push(env("metrics", 2)).await; // 3 + 2 = 5 恰好
        assert_eq!(buf.total_records().await, 5);
        assert_eq!(buf.len().await, 2);
    }

    /// 淘汰必须**返回条数**：只在日志里出现等于看不见（调用方据此计入自监控）。
    #[tokio::test]
    async fn overflow_reports_dropped_records() {
        let buf = Buffer::new(5);
        assert_eq!(buf.push(env("ebpf_edges", 3)).await, 0, "未超容量不淘汰");
        // 3 + 4 > 5：淘汰最旧那批（3 条）。
        assert_eq!(buf.push(env("ebpf_edges", 4)).await, 3);
        assert_eq!(buf.total_records().await, 4);
        // 一次入队淘汰两批：5 -> 淘汰 2 + 3，剩 6。
        let buf = Buffer::new(6);
        buf.push(env("a", 2)).await;
        buf.push(env("b", 3)).await;
        assert_eq!(buf.push(env("c", 6)).await, 5);
        assert_eq!(buf.total_records().await, 6);
        assert_eq!(buf.len().await, 1, "只剩最新那批");
    }

    #[tokio::test]
    async fn push_front_restores_retry_batch() {
        let buf = Buffer::new(100);
        buf.push(env("metrics", 1)).await;
        let head = buf.pop_front().await.expect("head");
        buf.push_front(head).await;
        assert_eq!(buf.total_records().await, 1);
        assert_eq!(buf.len().await, 1);
    }
}

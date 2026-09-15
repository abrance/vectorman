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
    pub async fn push(&self, env: DataEnvelope) {
        let mut guard = self.inner.lock().await;
        while !guard.queue.is_empty()
            && guard.total_records + env.record_count() > self.max_records
        {
            if let Some(dropped) = guard.queue.pop_front() {
                guard.total_records -= dropped.record_count();
                eprintln!(
                    "gse-agent: drop oldest batch data_type={} records={}",
                    dropped.data_type,
                    dropped.record_count()
                );
            }
        }
        guard.total_records += env.record_count();
        guard.queue.push_back(env);
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

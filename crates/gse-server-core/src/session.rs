use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use geminio::End;
use tokio::sync::Mutex;

/// 会话状态机。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Online,
    Checking,
    Offline,
    Closed,
}

/// 单个 Agent 会话，agent-id 为唯一标识。
#[derive(Debug, Clone)]
pub struct Session {
    pub agent_id: String,
    pub end: End,
    pub state: SessionState,
    pub last_seen_micros: i64,
    pub connected_at_micros: i64,
}

impl Session {
    pub fn new(agent_id: impl Into<String>, end: End, now_micros: i64) -> Self {
        let agent_id = agent_id.into();
        Self {
            agent_id,
            end,
            state: SessionState::Online,
            last_seen_micros: now_micros,
            connected_at_micros: now_micros,
        }
    }

    pub fn touch(&mut self, now_micros: i64) {
        self.last_seen_micros = now_micros;
    }

    /// 超时窗口内未收到任何消息时推进状态：Online → Checking → Offline。
    pub fn advance(&mut self, now_micros: i64, timeout_window_micros: i64) {
        if now_micros - self.last_seen_micros < timeout_window_micros {
            return;
        }
        self.state = match self.state {
            SessionState::Online => SessionState::Checking,
            SessionState::Checking => SessionState::Offline,
            other => other,
        };
    }

    pub fn close(&mut self) {
        self.state = SessionState::Closed;
    }
}

/// agent-id → Session 的内存注册表，保证每个 agent-id 至多一个活跃会话。
#[derive(Default)]
pub struct SessionRegistry {
    inner: Mutex<HashMap<String, Session>>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 插入或替换同名会话，返回被替换的旧会话。
    pub async fn insert(&self, session: Session) -> Option<Session> {
        self.inner
            .lock()
            .await
            .insert(session.agent_id.clone(), session)
    }

    pub async fn get(&self, agent_id: &str) -> Option<Session> {
        self.inner.lock().await.get(agent_id).cloned()
    }

    pub async fn remove(&self, agent_id: &str) -> Option<Session> {
        self.inner.lock().await.remove(agent_id)
    }

    pub async fn list(&self) -> Vec<Session> {
        self.inner.lock().await.values().cloned().collect()
    }

    /// 原位更新会话的 last_seen。
    pub async fn touch(&self, agent_id: &str, now_micros: i64) -> bool {
        let mut guard = self.inner.lock().await;
        if let Some(s) = guard.get_mut(agent_id) {
            s.touch(now_micros);
            true
        } else {
            false
        }
    }

    /// 对全部会话执行一次状态推进。
    pub async fn advance_all(&self, now_micros: i64, timeout_window_micros: i64) {
        let mut guard = self.inner.lock().await;
        for s in guard.values_mut() {
            s.advance(now_micros, timeout_window_micros);
        }
    }
}

pub fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

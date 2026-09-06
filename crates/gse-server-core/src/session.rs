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

    /// 强制更新会话状态，供上层模块与测试同步状态。
    pub async fn set_state(&self, agent_id: &str, state: SessionState) -> bool {
        let mut guard = self.inner.lock().await;
        if let Some(s) = guard.get_mut(agent_id) {
            s.state = state;
            true
        } else {
            false
        }
    }
}

pub fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use geminio::{dial, DialOptions, End, EndListener, ListenOptions};

    use super::*;

    /// 通过真实 loopback 连接构造一对 End，用于纯内存注册表测试。
    async fn end_pair() -> (End, End) {
        let listener = EndListener::bind("127.0.0.1:0", ListenOptions::default())
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let server_task = tokio::spawn(async move {
            let (end, _drivers) = listener.accept().await.expect("accept");
            end
        });
        let (client, _client_drivers) = dial(addr, DialOptions::default()).await.expect("dial");
        let server_end = server_task.await.expect("accept task");
        (server_end, client)
    }

    async fn session_fixture(agent_id: &str, now: i64) -> Session {
        let (_server, client) = end_pair().await;
        Session::new(agent_id, client, now)
    }

    #[tokio::test]
    async fn session_created_online() {
        let s = session_fixture("web-01", now_micros()).await;
        assert_eq!(s.state, SessionState::Online);
        assert_eq!(s.last_seen_micros, s.connected_at_micros);
    }

    #[tokio::test]
    async fn touch_refreshes_last_seen() {
        let mut s = session_fixture("web-01", 1_000).await;
        s.touch(2_000);
        assert_eq!(s.last_seen_micros, 2_000);
    }

    #[tokio::test]
    async fn advance_within_window_keeps_state() {
        let mut s = session_fixture("web-01", 1_000).await;
        s.state = SessionState::Checking;
        s.advance(1_050, 100);
        assert_eq!(s.state, SessionState::Checking);
    }

    #[tokio::test]
    async fn advance_online_to_checking() {
        let mut s = session_fixture("web-01", 1_000).await;
        s.advance(1_100, 100);
        assert_eq!(s.state, SessionState::Checking);
    }

    #[tokio::test]
    async fn advance_checking_to_offline() {
        let mut s = session_fixture("web-01", 1_000).await;
        s.state = SessionState::Checking;
        s.advance(1_100, 100);
        assert_eq!(s.state, SessionState::Offline);
    }

    #[tokio::test]
    async fn advance_offline_stays_offline() {
        let mut s = session_fixture("web-01", 1_000).await;
        s.state = SessionState::Offline;
        s.advance(1_100, 100);
        assert_eq!(s.state, SessionState::Offline);
    }

    #[tokio::test]
    async fn advance_closed_stays_closed() {
        let mut s = session_fixture("web-01", 1_000).await;
        s.close();
        s.advance(1_100, 100);
        assert_eq!(s.state, SessionState::Closed);
    }

    #[tokio::test]
    async fn close_marks_closed() {
        let mut s = session_fixture("web-01", 1_000).await;
        s.close();
        assert_eq!(s.state, SessionState::Closed);
    }

    #[tokio::test]
    async fn registry_insert_replace_keeps_single_active_session() {
        let registry = SessionRegistry::new();
        let base = session_fixture("web-01", 1_000).await;
        let replaced = registry.insert(base).await;
        assert!(replaced.is_none());
        assert_eq!(registry.list().await.len(), 1);

        let newer = session_fixture("web-01", 2_000).await;
        let old = registry.insert(newer.clone()).await.expect("old replaced");
        assert_eq!(old.connected_at_micros, 1_000);
        assert_eq!(registry.list().await.len(), 1);
        let current = registry.get("web-01").await.expect("exists");
        assert_eq!(current.connected_at_micros, 2_000);
    }

    #[tokio::test]
    async fn registry_distinct_agents_coexist() {
        let registry = SessionRegistry::new();
        registry
            .insert(session_fixture("web-01", 1_000).await)
            .await;
        registry
            .insert(session_fixture("web-02", 1_000).await)
            .await;
        assert_eq!(registry.list().await.len(), 2);
    }

    #[tokio::test]
    async fn registry_touch_and_advance_all() {
        let registry = SessionRegistry::new();
        registry
            .insert(session_fixture("web-01", 1_000).await)
            .await;
        registry
            .insert(session_fixture("web-02", 1_000).await)
            .await;

        registry.touch("web-01", 5_000).await;
        registry.advance_all(5_099, 100).await;

        let sessions = registry.list().await;
        let s1 = sessions.iter().find(|s| s.agent_id == "web-01").unwrap();
        assert_eq!(
            s1.state,
            SessionState::Online,
            "recently touched stays online"
        );
        let s2 = sessions.iter().find(|s| s.agent_id == "web-02").unwrap();
        assert_eq!(
            s2.state,
            SessionState::Checking,
            "idle session moves to checking"
        );

        registry.touch("web-01", 5_150).await;
        registry.advance_all(5_200, 100).await;

        let s1 = registry.get("web-01").await.unwrap();
        assert_eq!(s1.state, SessionState::Online, "touched again stays online");
        let s2 = registry.get("web-02").await.unwrap();
        assert_eq!(
            s2.state,
            SessionState::Offline,
            "idle session moves to offline"
        );
    }

    #[tokio::test]
    async fn registry_set_state_affects_existing_session() {
        let registry = SessionRegistry::new();
        registry
            .insert(session_fixture("web-01", 1_000).await)
            .await;
        assert!(registry.set_state("web-01", SessionState::Offline).await);
        assert_eq!(
            registry.get("web-01").await.unwrap().state,
            SessionState::Offline
        );
        assert!(!registry.set_state("ghost", SessionState::Offline).await);
    }

    #[test]
    fn now_micros_is_positive_and_increasing() {
        let a = now_micros();
        std::thread::sleep(Duration::from_millis(5));
        let b = now_micros();
        assert!(a > 0);
        assert!(b >= a);
    }
}

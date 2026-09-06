use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use geminio::Bytes;
use gse_agent_core::{run as run_agent, AgentConfig};
use gse_server_core::{
    http_router, AdminState, Agent, AgentConfig as LedgerAgentConfig, Server, ServerConfig,
    SessionState,
};
use tower::ServiceExt;

fn tmp_db(name: &str) -> String {
    std::env::temp_dir()
        .join(format!("gse-e2e-{}-{name}.db", std::process::id()))
        .to_string_lossy()
        .into_owned()
}

fn server_config(db: &str, auth_enabled: bool, timeout_secs: u64) -> ServerConfig {
    ServerConfig {
        listen: "127.0.0.1:0".to_string(),
        auth_enabled,
        db: db.to_string(),
        http_enabled: false,
        http_listen: "127.0.0.1:0".to_string(),
        heartbeat_interval_secs: 1,
        heartbeat_timeout_secs: timeout_secs,
    }
}

async fn register(server: &Server, agent_id: &str, token: &str) {
    server
        .ledger
        .upsert_agent(&Agent {
            agent_id: agent_id.to_string(),
            host_id: "h-1".to_string(),
            access_point_id: None,
            token: token.to_string(),
            version: "test".to_string(),
            install_path: String::new(),
            status: "unknown".to_string(),
            last_heartbeat_at: None,
            registered_at: String::new(),
        })
        .await
        .expect("register agent");
}

async fn wait_online(server: &Server, agent_id: &str) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let sessions = server.sessions().await;
            if sessions
                .iter()
                .any(|s| s.agent_id == agent_id && s.state == SessionState::Online)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("agent never came online");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_auth_heartbeat_ping_pong_update_ledger() {
    let db = tmp_db("ping-pong");
    let (server, addr) = Server::bind(server_config(&db, true, 5))
        .await
        .expect("bind");
    register(&server, "web-01", "tok-1").await;
    let server_ref = server.clone();
    tokio::spawn(async move {
        let _ = server_ref.run().await;
    });

    let cfg = AgentConfig {
        server_addr: addr.to_string(),
        agent_id: "web-01".to_string(),
        token: "tok-1".to_string(),
        heartbeat_interval_secs: 1,
    };
    tokio::spawn(run_agent(cfg));

    wait_online(&server, "web-01").await;

    // 认证通过后 ledger 状态回到 online 并记录心跳。
    let agent = server
        .ledger
        .get_agent("web-01")
        .await
        .expect("get agent")
        .expect("registered");
    assert_eq!(agent.status, "online");
    let first_beat = agent.last_heartbeat_at.expect("heartbeat recorded");

    let receipt = server
        .send_command("web-01", "ping", Bytes::new())
        .await
        .expect("send_command should succeed");
    assert!(receipt.ok, "ping should be accepted: {receipt:?}");
    assert_eq!(receipt.message.as_deref(), Some("pong"));
    assert!(!receipt.command_id.is_empty());

    // 心跳持续推进 last_heartbeat。
    tokio::time::sleep(Duration::from_secs(3)).await;
    let agent = server
        .ledger
        .get_agent("web-01")
        .await
        .expect("get agent")
        .expect("registered");
    let later = agent.last_heartbeat_at.expect("heartbeat recorded");
    assert!(later > first_beat, "heartbeat should advance in time");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_unknown_command_rejected() {
    let db = tmp_db("unknown-cmd");
    let (server, addr) = Server::bind(server_config(&db, true, 5))
        .await
        .expect("bind");
    register(&server, "web-01", "tok-1").await;
    let server_ref = server.clone();
    tokio::spawn(async move {
        let _ = server_ref.run().await;
    });

    let cfg = AgentConfig {
        server_addr: addr.to_string(),
        agent_id: "web-01".to_string(),
        token: "tok-1".to_string(),
        heartbeat_interval_secs: 1,
    };
    tokio::spawn(run_agent(cfg));

    wait_online(&server, "web-01").await;

    let receipt = server
        .send_command("web-01", "bogus", Bytes::new())
        .await
        .expect("send_command should succeed");
    assert!(!receipt.ok, "unknown command should be rejected");
    assert_eq!(receipt.message.as_deref(), Some("unknown command: bogus"));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_command_to_unknown_agent_fails() {
    let db = tmp_db("unknown-agent");
    let (server, _addr) = Server::bind(server_config(&db, true, 5))
        .await
        .expect("bind");
    let err = server
        .send_command("ghost", "ping", Bytes::new())
        .await
        .expect_err("unknown agent should be unavailable");
    assert_eq!(err.code, "unavailable");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_auth_rejected_agent_exits() {
    let db = tmp_db("auth-rejected");
    let (server, addr) = Server::bind(server_config(&db, true, 5))
        .await
        .expect("bind");
    register(&server, "web-01", "tok-1").await;
    let server_ref = server.clone();
    tokio::spawn(async move {
        let _ = server_ref.run().await;
    });

    let bad_cfg = AgentConfig {
        server_addr: addr.to_string(),
        agent_id: "web-01".to_string(),
        token: "wrong-token".to_string(),
        heartbeat_interval_secs: 1,
    };
    let task = tokio::spawn(run_agent(bad_cfg));
    let joined = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("agent should exit on auth failure")
        .expect("task should not panic");
    assert!(joined.is_err(), "rejected agent must return Err");

    let sessions = server.sessions().await;
    assert!(
        sessions.iter().all(|s| s.agent_id != "web-01"),
        "no session should exist for rejected agent"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_auth_unregistered_agent_exits() {
    let db = tmp_db("auth-unregistered");
    let (server, addr) = Server::bind(server_config(&db, true, 5))
        .await
        .expect("bind");
    let server_ref = server.clone();
    tokio::spawn(async move {
        let _ = server_ref.run().await;
    });

    let unknown_cfg = AgentConfig {
        server_addr: addr.to_string(),
        agent_id: "ghost".to_string(),
        token: "any-token".to_string(),
        heartbeat_interval_secs: 1,
    };
    let task = tokio::spawn(run_agent(unknown_cfg));
    let joined = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("agent should exit on unregistered id")
        .expect("task should not panic");
    assert!(joined.is_err(), "unregistered agent must return Err");

    let sessions = server.sessions().await;
    assert!(
        sessions.iter().all(|s| s.agent_id != "ghost"),
        "no session should exist for unregistered agent"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_auth_disabled_allows_unregistered_agent() {
    let db = tmp_db("auth-disabled");
    // auth_enabled=false：跳过 token 校验，未登记 agent 也可接入。
    let (server, addr) = Server::bind(server_config(&db, false, 5))
        .await
        .expect("bind");
    let server_ref = server.clone();
    tokio::spawn(async move {
        let _ = server_ref.run().await;
    });

    let cfg = AgentConfig {
        server_addr: addr.to_string(),
        agent_id: "ghost".to_string(),
        token: "any".to_string(),
        heartbeat_interval_secs: 1,
    };
    let handle = tokio::spawn(run_agent(cfg));

    wait_online(&server, "ghost").await;
    let receipt = server
        .send_command("ghost", "ping", Bytes::new())
        .await
        .expect("auth-disabled agent must answer");
    assert!(receipt.ok);
    assert_eq!(receipt.message.as_deref(), Some("pong"));

    handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_double_auth_keeps_single_session() {
    let db = tmp_db("double-auth");
    let (server, addr) = Server::bind(server_config(&db, true, 5))
        .await
        .expect("bind");
    register(&server, "web-01", "tok-1").await;
    let server_ref = server.clone();
    tokio::spawn(async move {
        let _ = server_ref.run().await;
    });

    let mk_cfg = || AgentConfig {
        server_addr: addr.to_string(),
        agent_id: "web-01".to_string(),
        token: "tok-1".to_string(),
        heartbeat_interval_secs: 1,
    };
    let h1 = tokio::spawn(run_agent(mk_cfg()));
    let h2 = tokio::spawn(run_agent(mk_cfg()));

    wait_online(&server, "web-01").await;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let sessions = server.sessions().await;
    let mine: Vec<_> = sessions.iter().filter(|s| s.agent_id == "web-01").collect();
    assert_eq!(mine.len(), 1, "at most one active session per agent-id");

    let receipt = server
        .send_command("web-01", "ping", Bytes::new())
        .await
        .expect("surviving session must answer");
    assert!(receipt.ok);

    h1.abort();
    h2.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_command_to_offline_session_unavailable() {
    let db = tmp_db("offline-session");
    let (server, addr) = Server::bind(server_config(&db, true, 3600))
        .await
        .expect("bind");
    register(&server, "web-01", "tok-1").await;
    let server_ref = server.clone();
    tokio::spawn(async move {
        let _ = server_ref.run().await;
    });

    let agent_cfg = AgentConfig {
        server_addr: addr.to_string(),
        agent_id: "web-01".to_string(),
        token: "tok-1".to_string(),
        heartbeat_interval_secs: 60,
    };
    let handle = tokio::spawn(run_agent(agent_cfg));
    wait_online(&server, "web-01").await;

    server
        .registry
        .set_state("web-01", SessionState::Offline)
        .await;
    let err = server
        .send_command("web-01", "ping", Bytes::new())
        .await
        .expect_err("offline agent must be unavailable");
    assert_eq!(err.code, "unavailable");

    server
        .registry
        .set_state("web-01", SessionState::Checking)
        .await;
    let err2 = server
        .send_command("web-01", "ping", Bytes::new())
        .await
        .expect_err("checking agent must be unavailable");
    assert_eq!(err2.code, "unavailable");

    handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_agent_reconnects_after_disconnect() {
    let db = tmp_db("reconnect");
    let (server, addr) = Server::bind(server_config(&db, true, 60))
        .await
        .expect("bind");
    register(&server, "web-01", "tok-1").await;
    let server_ref = server.clone();
    tokio::spawn(async move {
        let _ = server_ref.run().await;
    });

    let agent_cfg = AgentConfig {
        server_addr: addr.to_string(),
        agent_id: "web-01".to_string(),
        token: "tok-1".to_string(),
        heartbeat_interval_secs: 1,
    };
    let handle = tokio::spawn(run_agent(agent_cfg));

    wait_online(&server, "web-01").await;
    let original = server
        .sessions()
        .await
        .into_iter()
        .find(|s| s.agent_id == "web-01")
        .expect("session present");
    let original_connected = original.connected_at_micros;

    server
        .registry
        .get("web-01")
        .await
        .expect("session present")
        .end
        .close()
        .await
        .expect("close end");

    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let sessions = server.sessions().await;
            if let Some(s) = sessions.iter().find(|s| s.agent_id == "web-01") {
                if s.state == SessionState::Online && s.connected_at_micros > original_connected {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("agent should reconnect and reauthenticate");

    let receipt = server
        .send_command("web-01", "ping", Bytes::new())
        .await
        .expect("reconnected session must answer");
    assert!(receipt.ok);
    assert_eq!(receipt.message.as_deref(), Some("pong"));

    handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_liveness_marks_agent_offline_in_ledger() {
    let db = tmp_db("liveness-offline");
    let (server, addr) = Server::bind(server_config(&db, true, 1))
        .await
        .expect("bind");
    register(&server, "web-01", "tok-1").await;
    let server_ref = server.clone();
    tokio::spawn(async move {
        let _ = server_ref.run().await;
    });

    let agent_cfg = AgentConfig {
        server_addr: addr.to_string(),
        agent_id: "web-01".to_string(),
        token: "tok-1".to_string(),
        heartbeat_interval_secs: 1,
    };
    let handle = tokio::spawn(run_agent(agent_cfg));

    wait_online(&server, "web-01").await;
    assert_eq!(
        server
            .ledger
            .get_agent("web-01")
            .await
            .expect("get")
            .expect("exists")
            .status,
        "online"
    );

    // 停止 agent 心跳，等待 liveness 连续两个扫描周期推进到 Offline。
    handle.abort();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let sessions = server.sessions().await;
            if sessions
                .iter()
                .any(|s| s.agent_id == "web-01" && s.state == SessionState::Offline)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("session never reached offline");

    let agent = server
        .ledger
        .get_agent("web-01")
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(agent.status, "offline");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_http_delete_agent_clears_ledger_and_session() {
    let db = tmp_db("http-delete");
    let (server, addr) = Server::bind(server_config(&db, true, 5))
        .await
        .expect("bind");
    register(&server, "web-01", "tok-1").await;
    server
        .ledger
        .upsert_agent_config(&LedgerAgentConfig {
            agent_id: "web-01".to_string(),
            host_id: "h-1".to_string(),
            cpu_limit_percent: None,
            mem_limit_percent: None,
            log_level: "info".to_string(),
            updated_at: String::new(),
        })
        .await
        .expect("write agent config");
    let server_ref = server.clone();
    tokio::spawn(async move {
        let _ = server_ref.run().await;
    });

    let agent_cfg = AgentConfig {
        server_addr: addr.to_string(),
        agent_id: "web-01".to_string(),
        token: "tok-1".to_string(),
        heartbeat_interval_secs: 1,
    };
    let handle = tokio::spawn(run_agent(agent_cfg));
    wait_online(&server, "web-01").await;

    let app = http_router(AdminState {
        ledger: server.ledger.clone(),
        registry: Some(server.registry.clone()),
    })
    .into_service();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/agents/web-01")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("delete response");
    assert_eq!(resp.status(), StatusCode::OK);

    // 台账级联清空：agents + agent_configs。
    assert!(server
        .ledger
        .get_agent("web-01")
        .await
        .expect("get")
        .is_none());
    assert!(
        server
            .ledger
            .get_agent_config("web-01")
            .await
            .expect("get")
            .is_none(),
        "agent config should be cascaded away"
    );
    // 活跃会话被移除 -> 指令不可达。
    let err = server
        .send_command("web-01", "ping", Bytes::new())
        .await
        .expect_err("deleted agent must be unavailable");
    assert_eq!(err.code, "unavailable");

    handle.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_bind_registers_access_point_idempotently() {
    let db = tmp_db("self-register");
    let cfg = server_config(&db, true, 5);
    let (server, _addr) = Server::bind(cfg.clone()).await.expect("bind");
    let points = server.ledger.list_access_points().await.expect("list");
    assert_eq!(points.len(), 1, "one access point per bind");
    assert_eq!(points[0].id, format!("gse-server:{}", cfg.listen));

    // 再次 bind 同配置 -> 幂等覆盖，不产生第二条记录。
    let (server2, _addr2) = Server::bind(cfg).await.expect("bind second");
    let points = server2.ledger.list_access_points().await.expect("list");
    assert_eq!(points.len(), 1, "re-bind should stay idempotent");
    assert_eq!(
        points[0].id,
        format!("gse-server:{}", server_config(&db, true, 5).listen)
    );
}

use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use geminio::Bytes;
use gse_agent_core::{run as run_agent, AgentConfig};
use gse_proto::{FileEndpoint, JobStatus};
use gse_server_core::file_transfer::{submit_file_job, FileJobSubmit};
use gse_server_core::{
    http_router, AdminState, Agent, AgentConfig as LedgerAgentConfig, JobRecord, JobSubmit, Ledger,
    NewJob, Server, ServerConfig, SessionState,
};
use http_body_util::BodyExt;
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
        http_web_dir: None,
        heartbeat_interval_secs: 1,
        heartbeat_timeout_secs: timeout_secs,
        metrics_listen: "127.0.0.1:0".to_string(),
        ..Default::default()
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

/// 会话生命周期回归（本次事故核心）：**连接断开后会话必须被清理**。
///
/// 要用真 `Server::run()` 跑服务端（这样才能覆盖 `handle_conn` 的真实生命周期），
/// 但客户端用裸 geminio —— `run_agent` 的 driver 是独立 spawn 的，abort 它
/// 不会断开连接（实测探测会一直成功），无法构造「连接断开」这个场景。
/// 裸客户端的 `End` 一 drop，连接即断。
#[tokio::test(flavor = "multi_thread")]
async fn e2e_session_cleaned_after_connection_ends() {
    use geminio::{dial, DialOptions};

    let db = tmp_db("session-cleanup");
    let (server, addr) = Server::bind(server_config(&db, true, 30))
        .await
        .expect("bind");
    register(&server, "web-01", "tok-1").await;
    let server_ref = server.clone();
    tokio::spawn(async move {
        let _ = server_ref.run().await;
    });

    // 客户端连上并认证 —— 与真 agent 的认证路径一致。
    let (client, client_drivers) = dial(addr.to_string(), DialOptions::default())
        .await
        .expect("dial");
    // 服务端的 handle_conn 会注册若干 handler，每个都要对端回 RegisterAck。
    // 裸客户端不注册任何 handler 时，dispatcher 无路由可用、ack 不发出，
    // 服务端的 register 会永久挂起 —— 这是测试构造问题（真 agent 会注册）。
    // 这里注册足量的 handler 让流程走完。
    for m in [
        "job_exec",
        "file_read",
        "file_write",
        "collect_items",
        "exec",
    ] {
        client
            .register(m, |_r: Bytes| async move { Ok(Bytes::new()) })
            .await
            .expect("register");
    }
    let resp = client
        .call(
            "auth",
            Bytes::from(r#"{"agent_id":"web-01","token":"tok-1"}"#),
        )
        .await
        .expect("auth rpc");
    let reply: serde_json::Value = serde_json::from_slice(&resp).expect("reply");
    assert_eq!(reply["ok"], true, "auth should pass: {reply}");

    wait_online(&server, "web-01").await;

    // 断开连接：drop End 与 drivers。
    drop(client);
    drop(client_drivers);

    // 会话应在有限时间内被清理（探测间隔 15s + 超时 5s，留足余量）。
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            if server
                .sessions()
                .await
                .iter()
                .all(|s| s.agent_id != "web-01")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("连接断开后会话必须被清理（僵尸会话回归）");

    let agent = server
        .ledger
        .get_agent("web-01")
        .await
        .expect("get agent")
        .expect("agent exists");
    assert_eq!(agent.status, "offline", "断开后台账应为 offline");
}

/// 重连后会话必须重建、台账回到 online（同样不重启 server）。
#[tokio::test(flavor = "multi_thread")]
async fn e2e_session_rebuilds_after_reconnect() {
    use geminio::{dial, DialOptions};

    let db = tmp_db("session-rebuild");
    let (server, addr) = Server::bind(server_config(&db, true, 30))
        .await
        .expect("bind");
    register(&server, "web-01", "tok-1").await;
    let server_ref = server.clone();
    tokio::spawn(async move {
        let _ = server_ref.run().await;
    });

    let (client, client_drivers) = dial(addr.to_string(), DialOptions::default())
        .await
        .expect("dial 1");
    for m in [
        "job_exec",
        "file_read",
        "file_write",
        "collect_items",
        "exec",
    ] {
        client
            .register(m, |_r: Bytes| async move { Ok(Bytes::new()) })
            .await
            .expect("register");
    }
    client
        .call(
            "auth",
            Bytes::from(r#"{"agent_id":"web-01","token":"tok-1"}"#),
        )
        .await
        .expect("auth rpc");
    wait_online(&server, "web-01").await;

    drop(client);
    drop(client_drivers);
    tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            if server
                .sessions()
                .await
                .iter()
                .all(|s| s.agent_id != "web-01")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("会话应被清理");

    // 重连 → 会话重建、台账回 online。
    let (client2, client_drivers2) = dial(addr.to_string(), DialOptions::default())
        .await
        .expect("dial 2");
    for m in [
        "job_exec",
        "file_read",
        "file_write",
        "collect_items",
        "exec",
    ] {
        client2
            .register(m, |_r: Bytes| async move { Ok(Bytes::new()) })
            .await
            .expect("register");
    }
    client2
        .call(
            "auth",
            Bytes::from(r#"{"agent_id":"web-01","token":"tok-1"}"#),
        )
        .await
        .expect("auth rpc 2");
    wait_online(&server, "web-01").await;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(Some(a)) = server.ledger.get_agent("web-01").await {
                if a.status == "online" {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("重连后台账应回到 online");

    drop(client2);
    drop(client_drivers2);
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
    };
    let handle = tokio::spawn(run_agent(agent_cfg));
    wait_online(&server, "web-01").await;

    let app = http_router(
        AdminState {
            ledger: server.ledger.clone(),
            registry: Some(server.registry.clone()),
            cfg: Some(server.cfg.clone()),
            file_store: Some(server.file_store.clone()),
        },
        None,
    )
    .into_service();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/gse/agents/web-01")
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

#[tokio::test(flavor = "multi_thread")]
async fn malformed_connection_does_not_kill_server() {
    use tokio::io::AsyncWriteExt;

    let db = tmp_db("malformed-conn");
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
        ..Default::default()
    };
    tokio::spawn(run_agent(cfg));
    wait_online(&server, "web-01").await;

    // 注入非法 wire-format / 半包连接：不得使服务退出。
    for payload in [vec![0x16u8], vec![0x00, 0xff, 0xff, 0xff]] {
        let mut stream = tokio::net::TcpStream::connect(addr)
            .await
            .expect("connect to server");
        let _ = stream.write_all(&payload).await;
        let _ = stream.shutdown().await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 服务仍处理正常会话与指令。
    let receipt = server
        .send_command("web-01", "ping", Bytes::new())
        .await
        .expect("server should survive malformed connections");
    assert!(receipt.ok, "ping should still succeed: {receipt:?}");
    assert_eq!(receipt.message.as_deref(), Some("pong"));
}

fn job_submit(
    agent_id: &str,
    interpreter: Option<&str>,
    script: &str,
    timeout_secs: Option<u64>,
) -> JobSubmit {
    JobSubmit {
        agent_id: agent_id.to_string(),
        interpreter: interpreter.map(str::to_string),
        script: script.to_string(),
        args: vec![],
        env: std::collections::BTreeMap::new(),
        working_dir: None,
        timeout_secs,
    }
}

async fn wait_terminal(server: &Server, job_id: &str) -> JobRecord {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let job = server
                .ledger
                .get_job(job_id)
                .await
                .expect("get job")
                .expect("job exists");
            if job.status.is_terminal() {
                return job;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("job never reached a terminal state")
}

async fn spawn_server_and_agent(db: &str, cfg: AgentConfig) -> std::sync::Arc<Server> {
    spawn_server_and_agent_with(server_config(db, true, 60), cfg).await
}

async fn spawn_server_and_agent_with(
    scfg: ServerConfig,
    cfg: AgentConfig,
) -> std::sync::Arc<Server> {
    let (server, addr) = Server::bind(scfg).await.expect("bind");
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
        ..cfg
    };
    tokio::spawn(run_agent(cfg));
    wait_online(&server, "web-01").await;
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_job_lifecycle_success_failure_timeout() {
    let server = spawn_server_and_agent(&tmp_db("job-life"), AgentConfig::default()).await;

    let ok = server
        .submit_job(job_submit("web-01", None, "echo hello", None))
        .await
        .expect("submit");
    let ok = wait_terminal(&server, &ok.job_id).await;
    assert_eq!(ok.status, JobStatus::Succeeded, "{ok:?}");
    assert_eq!(ok.exit_code, Some(0));
    assert_eq!(ok.stdout.as_deref().map(str::trim), Some("hello"));

    let failed = server
        .submit_job(job_submit("web-01", None, "echo bad >&2; exit 3", None))
        .await
        .expect("submit");
    let failed = wait_terminal(&server, &failed.job_id).await;
    assert_eq!(failed.status, JobStatus::Failed, "{failed:?}");
    assert_eq!(failed.exit_code, Some(3));
    assert_eq!(failed.stderr.as_deref().map(str::trim), Some("bad"));

    let timeout = server
        .submit_job(job_submit("web-01", None, "sleep 30; echo done", Some(1)))
        .await
        .expect("submit");
    let timeout = wait_terminal(&server, &timeout.job_id).await;
    assert_eq!(timeout.status, JobStatus::Timeout, "{timeout:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_job_rejected_when_interpreter_not_allowed() {
    let cfg = AgentConfig {
        allowed_interpreters: vec!["bash".to_string()],
        ..Default::default()
    };
    let server = spawn_server_and_agent(&tmp_db("job-reject"), cfg).await;

    let job = server
        .submit_job(job_submit("web-01", Some("python3"), "print(1)", None))
        .await
        .expect("submit");
    let job = wait_terminal(&server, &job.job_id).await;
    assert_eq!(job.status, JobStatus::Rejected, "{job:?}");
    assert!(
        job.error
            .as_deref()
            .unwrap_or_default()
            .contains("interpreter"),
        "{job:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_inflight_jobs_become_lost_after_restart() {
    let db = tmp_db("job-restart");
    {
        let ledger = Ledger::new(&db).expect("open ledger");
        ledger.init().await.expect("init");
        ledger
            .insert_job(&NewJob {
                job_id: "job-inflight".to_string(),
                agent_id: "web-01".to_string(),
                interpreter: "bash".to_string(),
                script: "sleep 100".to_string(),
                args: vec![],
                env: std::collections::BTreeMap::new(),
                working_dir: None,
                template_id: None,
                rerun_of: None,
                timeout_secs: 300,
                created_at: "1".to_string(),
                ..Default::default()
            })
            .await
            .expect("insert");
    }

    let (server, _addr) = Server::bind(server_config(&db, true, 5))
        .await
        .expect("rebind");
    let job = server
        .ledger
        .get_job("job-inflight")
        .await
        .expect("get")
        .expect("job");
    assert_eq!(job.status, JobStatus::Lost, "{job:?}");
}

async fn send_json(
    app: &mut axum::Router,
    method: &str,
    uri: &str,
    body: &str,
) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request");
    let resp = app.clone().oneshot(request).await.expect("oneshot");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn json_field(body: &str, key: &str) -> String {
    let needle = format!("\"{key}\":\"");
    body.split(&needle)
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_template_submit_and_save_as_template() {
    let server = spawn_server_and_agent(&tmp_db("job-template"), AgentConfig::default()).await;
    let mut app = http_router(
        AdminState {
            ledger: server.ledger.clone(),
            registry: Some(server.registry.clone()),
            cfg: Some(server.cfg.clone()),
            file_store: Some(server.file_store.clone()),
        },
        None,
    );

    // 创建含 ${svc} 的模板。
    let (status, body) = send_json(
        &mut app,
        "POST",
        "/api/gse/job-templates",
        r#"{"name":"echo-svc","script":"echo ${svc}","timeout_secs":30}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let template_id = json_field(&body, "template_id");
    assert!(!template_id.is_empty(), "{body}");

    // 用模板提交并展开变量。
    let (status, body) = send_json(
        &mut app,
        "POST",
        &format!("/api/gse/job-templates/{template_id}/submit"),
        r#"{"agent_id":"web-01","vars":{"svc":"nginx"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let job_id = json_field(&body, "job_id");
    let job = wait_terminal(&server, &job_id).await;
    assert_eq!(job.status, JobStatus::Succeeded, "{job:?}");
    assert_eq!(job.stdout.as_deref().map(str::trim), Some("nginx"));
    assert_eq!(job.template_id.as_deref(), Some(template_id.as_str()));

    // 另存为模板后再提交。
    let (status, body) = send_json(
        &mut app,
        "POST",
        &format!("/api/gse/jobs/{job_id}/save-as-template"),
        r#"{"name":"from-job"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let saved_id = json_field(&body, "template_id");
    assert!(!saved_id.is_empty(), "{body}");

    let (status, body) = send_json(
        &mut app,
        "POST",
        &format!("/api/gse/job-templates/{saved_id}/submit"),
        r#"{"agent_id":"web-01","vars":{}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let second_id = json_field(&body, "job_id");
    let second = wait_terminal(&server, &second_id).await;
    assert_eq!(second.status, JobStatus::Succeeded, "{second:?}");
    assert_eq!(second.stdout.as_deref().map(str::trim), Some("nginx"));
    assert_eq!(second.template_id.as_deref(), Some(saved_id.as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_rerun_history_job() {
    let server = spawn_server_and_agent(&tmp_db("job-rerun"), AgentConfig::default()).await;
    let mut app = http_router(
        AdminState {
            ledger: server.ledger.clone(),
            registry: Some(server.registry.clone()),
            cfg: Some(server.cfg.clone()),
            file_store: Some(server.file_store.clone()),
        },
        None,
    );

    // 提交来源作业并等待终态。
    let (status, body) = send_json(
        &mut app,
        "POST",
        "/api/gse/jobs",
        r#"{"agent_id":"web-01","script":"echo src","timeout_secs":30}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let source_id = json_field(&body, "job_id");
    let source = wait_terminal(&server, &source_id).await;
    assert_eq!(source.status, JobStatus::Succeeded, "{source:?}");
    assert_eq!(source.stdout.as_deref().map(str::trim), Some("src"));

    // 空体重做：继承来源参数，记录来源作业且无模板来源。
    let (status, body) = send_json(
        &mut app,
        "POST",
        &format!("/api/gse/jobs/{source_id}/rerun"),
        "{}",
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let rerun_id = json_field(&body, "job_id");
    assert_ne!(rerun_id, source_id);
    let rerun = wait_terminal(&server, &rerun_id).await;
    assert_eq!(rerun.status, JobStatus::Succeeded, "{rerun:?}");
    assert_eq!(rerun.stdout.as_deref().map(str::trim), Some("src"));
    assert_eq!(rerun.rerun_of.as_deref(), Some(source_id.as_str()));
    assert_eq!(rerun.template_id, None);

    // 编辑后重做：覆盖脚本，生成不同的 job_id。
    let (status, body) = send_json(
        &mut app,
        "POST",
        &format!("/api/gse/jobs/{source_id}/rerun"),
        r#"{"script":"echo edited"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let edited_id = json_field(&body, "job_id");
    assert_ne!(edited_id, rerun_id);
    let edited = wait_terminal(&server, &edited_id).await;
    assert_eq!(edited.status, JobStatus::Succeeded, "{edited:?}");
    assert_eq!(edited.stdout.as_deref().map(str::trim), Some("edited"));
    assert_eq!(edited.rerun_of.as_deref(), Some(source_id.as_str()));

    // 来源作业保持不变。
    let unchanged = server
        .ledger
        .get_job(&source_id)
        .await
        .expect("get")
        .expect("exists");
    assert_eq!(unchanged, source);
}

fn abs_tmp(name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("gse-e2e-ft-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir.join(name).to_string_lossy().into_owned()
}

fn file_job(source: FileEndpoint, destination: FileEndpoint) -> FileJobSubmit {
    FileJobSubmit {
        kind: "file_transfer".to_string(),
        source: Some(source),
        destination: Some(destination),
        timeout_secs: Some(30),
    }
}

async fn submit_file(server: &Server, req: FileJobSubmit) -> JobRecord {
    submit_file_job(
        server.ledger.clone(),
        server.registry.clone(),
        server.cfg.clone(),
        server.file_store.clone(),
        req,
        None,
    )
    .await
    .expect("submit file job")
}

fn agent_path(path: &str) -> FileEndpoint {
    FileEndpoint::Agent {
        agent_id: "web-01".to_string(),
        path: path.to_string(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_file_agent_to_agent() {
    let server = spawn_server_and_agent(&tmp_db("ft-aa"), AgentConfig::default()).await;
    let src = abs_tmp("src.bin");
    let dst = abs_tmp("dst.bin");
    std::fs::write(&src, b"payload-aa").expect("write src");
    let _ = std::fs::remove_file(&dst);
    let job = submit_file(&server, file_job(agent_path(&src), agent_path(&dst))).await;
    let job = wait_terminal(&server, &job.job_id).await;
    assert_eq!(job.status, JobStatus::Succeeded, "{job:?}");
    assert_eq!(std::fs::read(&dst).expect("read dst"), b"payload-aa");
    assert_eq!(job.file_name.as_deref(), Some("src.bin"));
    assert_eq!(job.file_bytes, Some(10));
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_file_agent_to_temp() {
    let server = spawn_server_and_agent(&tmp_db("ft-at"), AgentConfig::default()).await;
    let src = abs_tmp("pull.bin");
    std::fs::write(&src, b"from-agent").expect("write src");
    let job = submit_file(
        &server,
        file_job(agent_path(&src), FileEndpoint::ServerTemp { file_id: None }),
    )
    .await;
    let job = wait_terminal(&server, &job.job_id).await;
    assert_eq!(job.status, JobStatus::Succeeded, "{job:?}");
    let file_id = job.file_id.clone().expect("file_id");
    let (meta, data) = server.file_store.get(&file_id).expect("get temp");
    assert_eq!(data, b"from-agent");
    assert_eq!(meta.file_name, "pull.bin");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_file_temp_to_agent() {
    let server = spawn_server_and_agent(&tmp_db("ft-ta"), AgentConfig::default()).await;
    let meta = server.file_store.put("up.bin", b"uploaded").expect("put");
    let dst = abs_tmp("out.bin");
    let _ = std::fs::remove_file(&dst);
    let job = submit_file(
        &server,
        file_job(
            FileEndpoint::ServerTemp {
                file_id: Some(meta.file_id.clone()),
            },
            agent_path(&dst),
        ),
    )
    .await;
    let job = wait_terminal(&server, &job.job_id).await;
    assert_eq!(job.status, JobStatus::Succeeded, "{job:?}");
    assert_eq!(std::fs::read(&dst).expect("read dst"), b"uploaded");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_file_source_missing() {
    let server = spawn_server_and_agent(&tmp_db("ft-miss"), AgentConfig::default()).await;
    let src = abs_tmp("no-such.bin");
    let _ = std::fs::remove_file(&src);
    let dst = abs_tmp("miss-dst.bin");
    let _ = std::fs::remove_file(&dst);
    let job = submit_file(&server, file_job(agent_path(&src), agent_path(&dst))).await;
    let job = wait_terminal(&server, &job.job_id).await;
    assert_eq!(job.status, JobStatus::Failed, "{job:?}");
    assert!(
        job.error
            .as_deref()
            .unwrap_or_default()
            .contains("not_found"),
        "{job:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_file_too_large() {
    let mut scfg = server_config(&tmp_db("ft-big"), true, 60);
    scfg.job_max_file_bytes = 4;
    scfg.job_file_chunk_bytes = 4;
    let server = spawn_server_and_agent_with(scfg, AgentConfig::default()).await;
    let src = abs_tmp("big.bin");
    std::fs::write(&src, b"too-big").expect("write src");
    let dst = abs_tmp("big-dst.bin");
    let _ = std::fs::remove_file(&dst);
    let job = submit_file(&server, file_job(agent_path(&src), agent_path(&dst))).await;
    let job = wait_terminal(&server, &job.job_id).await;
    assert_eq!(job.status, JobStatus::Failed, "{job:?}");
    assert!(
        job.error
            .as_deref()
            .unwrap_or_default()
            .contains("file_too_large")
            || job.error.as_deref().unwrap_or_default().contains("exceeds"),
        "{job:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_file_dest_already_exists() {
    let server = spawn_server_and_agent(&tmp_db("ft-exists"), AgentConfig::default()).await;
    let src = abs_tmp("exists-src.bin");
    let dst = abs_tmp("exists-dst.bin");
    std::fs::write(&src, b"src").expect("write src");
    std::fs::write(&dst, b"keep").expect("write dst");
    let job = submit_file(&server, file_job(agent_path(&src), agent_path(&dst))).await;
    let job = wait_terminal(&server, &job.job_id).await;
    assert_eq!(job.status, JobStatus::Failed, "{job:?}");
    assert!(
        job.error
            .as_deref()
            .unwrap_or_default()
            .contains("already_exists"),
        "{job:?}"
    );
    assert_eq!(std::fs::read(&dst).expect("read dst"), b"keep");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_file_missing_file_id_is_404() {
    let server = spawn_server_and_agent(&tmp_db("ft-404"), AgentConfig::default()).await;
    let mut app = http_router(
        AdminState {
            ledger: server.ledger.clone(),
            registry: Some(server.registry.clone()),
            cfg: Some(server.cfg.clone()),
            file_store: Some(server.file_store.clone()),
        },
        None,
    );
    let dst = abs_tmp("404-dst.bin");
    let (status, body) = send_json(
        &mut app,
        "POST",
        "/api/gse/jobs",
        &format!(
            r#"{{"kind":"file_transfer","source":{{"type":"server_temp","file_id":"file-nope"}},"destination":{{"type":"agent","agent_id":"web-01","path":"{dst}"}}}}"#
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

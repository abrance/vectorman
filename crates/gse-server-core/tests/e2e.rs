use std::collections::HashMap;
use std::time::Duration;

use geminio::Bytes;
use gse_agent_core::{run as run_agent, AgentConfig};
use gse_server_core::{Server, ServerConfig, SessionState};

fn server_config() -> ServerConfig {
    let mut agents = HashMap::new();
    agents.insert("web-01".to_string(), "tok-1".to_string());
    ServerConfig {
        listen: "127.0.0.1:0".to_string(),
        auth_enabled: true,
        agents,
        heartbeat_interval_secs: 1,
        heartbeat_timeout_secs: 5,
    }
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
async fn e2e_auth_heartbeat_ping_pong() {
    let (server, addr) = Server::bind(server_config()).await.expect("bind");
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
        .send_command("web-01", "ping", Bytes::new())
        .await
        .expect("send_command should succeed");
    assert!(receipt.ok, "ping should be accepted: {receipt:?}");
    assert_eq!(receipt.message.as_deref(), Some("pong"));
    assert!(!receipt.command_id.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_unknown_command_rejected() {
    let (server, addr) = Server::bind(server_config()).await.expect("bind");
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
    let (server, _addr) = Server::bind(server_config()).await.expect("bind");
    let err = server
        .send_command("ghost", "ping", Bytes::new())
        .await
        .expect_err("unknown agent should be unavailable");
    assert_eq!(err.code, "unavailable");
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_auth_rejected_agent_exits() {
    let (server, addr) = Server::bind(server_config()).await.expect("bind");
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
    let (server, addr) = Server::bind(server_config()).await.expect("bind");
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
async fn e2e_double_auth_keeps_single_session() {
    let (server, addr) = Server::bind(server_config()).await.expect("bind");
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
    let cfg = ServerConfig {
        heartbeat_timeout_secs: 3600,
        ..server_config()
    };
    let (server, addr) = Server::bind(cfg).await.expect("bind");
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
    let cfg = ServerConfig {
        heartbeat_timeout_secs: 60,
        ..server_config()
    };
    let (server, addr) = Server::bind(cfg).await.expect("bind");
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

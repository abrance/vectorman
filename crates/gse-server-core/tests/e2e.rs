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

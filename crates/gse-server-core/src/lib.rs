//! gse-server 核心库：配置、会话注册表、认证/心跳/信令与存活检测。
//! bins/gse-server 仅作为进程入口调用本库。

pub mod config;
pub mod http;
pub mod ledger;
pub mod server;
pub mod session;

pub use config::{load_config, ServerConfig};
pub use http::{router as http_router, AdminState};
pub use ledger::{AccessPoint, Agent, AgentConfig, Host, Ledger};
pub use server::Server;
pub use session::{now_micros, Session, SessionRegistry, SessionState};

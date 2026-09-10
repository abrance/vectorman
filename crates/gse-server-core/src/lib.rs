//! gse-server 核心库：配置、会话注册表、认证/心跳/信令与存活检测。
//! bins/gse-server 仅作为进程入口调用本库。

pub mod config;
pub mod http;
pub mod ledger;
pub mod rerun;
pub mod server;
pub mod session;
pub mod template;

pub use config::{load_config, ServerConfig};
pub use http::{router as http_router, AdminState};
pub use ledger::{
    AccessPoint, Agent, AgentConfig, Host, JobRecord, JobTemplate, Ledger, NewJob, NewJobTemplate,
};
pub use rerun::{build_rerun_submit, RerunRequest};
pub use server::{
    handle_job_result, submit_job, submit_job_with_source, submit_job_with_template, submit_rerun,
    JobSubmit, Server,
};
pub use session::{now_micros, Session, SessionRegistry, SessionState};
pub use template::{expand, extract_variables, validate_placeholders, ExpandedJob, TemplateInput};

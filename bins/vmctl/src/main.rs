use std::process::ExitCode;

use clap::{Parser, Subcommand};
use vmctl::{Client, JobRerunSpec, JobSubmitSpec, UreqTransport, WaitPolicy, DEFAULT_BASE_URL};

#[derive(Parser)]
#[command(
    name = "vmctl",
    about = "gse-server HTTP 客户端：节点只读查询与作业提交"
)]
struct Cli {
    /// gse-server HTTP 根地址
    #[arg(long, default_value = DEFAULT_BASE_URL)]
    url: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// GET /health
    Health,
    /// Host 台账只读
    Hosts {
        #[command(subcommand)]
        cmd: HostsCmd,
    },
    /// Agent 台账只读
    Agents {
        #[command(subcommand)]
        cmd: AgentsCmd,
    },
    /// 作业提交、查询与重做
    Jobs {
        #[command(subcommand)]
        cmd: JobsCmd,
    },
}

#[derive(Subcommand)]
enum HostsCmd {
    List,
    Get { host_id: String },
}

#[derive(Subcommand)]
enum AgentsCmd {
    List,
    Get { agent_id: String },
}

#[derive(Subcommand)]
enum JobsCmd {
    List {
        #[arg(long)]
        agent_id: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        limit: Option<i64>,
    },
    Get {
        job_id: String,
    },
    Submit {
        #[arg(long)]
        agent_id: String,
        #[arg(long)]
        script_file: String,
        #[arg(long)]
        interpreter: Option<String>,
        #[arg(long = "arg")]
        args: Vec<String>,
        #[arg(long = "env")]
        env: Vec<String>,
        #[arg(long)]
        working_dir: Option<String>,
        #[arg(long)]
        timeout_secs: Option<u64>,
        #[arg(long)]
        wait: bool,
    },
    Rerun {
        job_id: String,
        #[arg(long)]
        agent_id: Option<String>,
        #[arg(long)]
        script_file: Option<String>,
        #[arg(long)]
        interpreter: Option<String>,
        #[arg(long = "arg")]
        args: Vec<String>,
        #[arg(long = "env")]
        env: Vec<String>,
        #[arg(long)]
        working_dir: Option<String>,
        #[arg(long)]
        timeout_secs: Option<u64>,
        #[arg(long)]
        wait: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let transport = UreqTransport::new();
    let client = Client {
        base_url: cli.url,
        transport: &transport,
        wait: WaitPolicy::default(),
    };
    let out = match cli.command {
        Command::Health => client.health(),
        Command::Hosts { cmd } => match cmd {
            HostsCmd::List => client.hosts_list(),
            HostsCmd::Get { host_id } => client.hosts_get(&host_id),
        },
        Command::Agents { cmd } => match cmd {
            AgentsCmd::List => client.agents_list(),
            AgentsCmd::Get { agent_id } => client.agents_get(&agent_id),
        },
        Command::Jobs { cmd } => match cmd {
            JobsCmd::List {
                agent_id,
                status,
                limit,
            } => client.jobs_list(agent_id.as_deref(), status.as_deref(), limit),
            JobsCmd::Get { job_id } => client.jobs_get(&job_id),
            JobsCmd::Submit {
                agent_id,
                script_file,
                interpreter,
                args,
                env,
                working_dir,
                timeout_secs,
                wait,
            } => client.jobs_submit(&JobSubmitSpec {
                agent_id,
                script_file,
                interpreter,
                args,
                env,
                working_dir,
                timeout_secs,
                wait,
            }),
            JobsCmd::Rerun {
                job_id,
                agent_id,
                script_file,
                interpreter,
                args,
                env,
                working_dir,
                timeout_secs,
                wait,
            } => client.jobs_rerun(&JobRerunSpec {
                job_id,
                agent_id,
                script_file,
                interpreter,
                args,
                env,
                working_dir,
                timeout_secs,
                wait,
            }),
        },
    };
    if !out.stdout.is_empty() {
        print!("{}", out.stdout);
        if !out.stdout.ends_with('\n') {
            println!();
        }
    }
    if !out.stderr.is_empty() {
        eprint!("{}", out.stderr);
        if !out.stderr.ends_with('\n') {
            eprintln!();
        }
    }
    ExitCode::from(out.code)
}

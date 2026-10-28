use clap::{Args, Parser, Subcommand};
use gtk_mcp::{Broker, Config, Policy, cli, doctor, server::Server};
use gtk_mcp_protocol::AgentError;
use rmcp::{ServiceExt, transport::stdio};
use serde_json::Value;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

/// GTK4 app inspection MCP server (stdio). Logs go to stderr; stdout is MCP only.
/// Without a subcommand it runs the server, like `serve`.
#[derive(Parser, Debug)]
#[command(name = "gtk-mcp", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    serve: ServeArgs,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the MCP server on stdio (the default).
    Serve(ServeArgs),
    /// Diagnose the socket directory, every app socket and the AT-SPI bus. Changes nothing.
    Doctor {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// List running instrumented apps.
    Apps {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// List registered app states, or read NAME as JSON.
    State {
        /// State name relative to app-state/ (see `gtk-mcp state`).
        name: Option<String>,
        /// App to read: session id, pid or app id.
        #[arg(long)]
        app: Option<String>,
        /// Machine-readable list output (values are always JSON).
        #[arg(long)]
        json: bool,
    },
    /// List tools grouped by area (R read-only, M mutating). With NAME, print that tool's
    /// description and ARGS_JSON schema.
    Tools {
        /// Tool to describe.
        name: Option<String>,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Run one tool and print its JSON result.
    Call(CallArgs),
    /// Print a guide for AI agents driving gtk-mcp from the shell.
    Prime,
}

#[derive(Args, Debug)]
struct CallArgs {
    /// Tool name (see `gtk-mcp tools`).
    tool: String,
    /// Tool arguments as a JSON object; `-` reads stdin. Default `{}`.
    args_json: Option<String>,
    /// App to run the tool on: session id, pid or app id. Fills `session` when ARGS_JSON has none.
    #[arg(long)]
    app: Option<String>,
    /// Write the PNG of a capture to FILE.
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    output: Option<PathBuf>,
    /// Allow launching and stopping apps.
    #[arg(long)]
    allow_launch: bool,
    /// Allow the AT-SPI source.
    #[arg(long)]
    allow_atspi: bool,
}

#[derive(Args, Debug)]
struct ServeArgs {
    /// Comma-separated exact tool names to expose; all other tools are hidden.
    #[arg(long, value_delimiter = ',')]
    tools: Option<Vec<String>>,
    /// Comma-separated exact tool names to hide.
    #[arg(long, value_delimiter = ',', default_value = "")]
    exclude_tools: Vec<String>,
    /// Allow launching and stopping apps.
    #[arg(long)]
    allow_launch: bool,
    /// Allow the AT-SPI source.
    #[arg(long)]
    allow_atspi: bool,
    /// Snapshots retained per session.
    #[arg(long, default_value_t = 16)]
    retain_snapshots: usize,
    /// Captures retained per session.
    #[arg(long, default_value_t = 16)]
    retain_captures: usize,
    /// Exited sessions kept listed.
    #[arg(long, default_value_t = 8)]
    retain_exited: usize,
}

fn names(v: Vec<String>) -> Vec<String> {
    v.into_iter().filter(|s| !s.is_empty()).collect()
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        None => finish(serve(cli.serve).await),
        Some(Command::Serve(a)) => finish(serve(a).await),
        Some(Command::Doctor { json }) => doctor_cmd(json).await,
        Some(Command::Tools { name: None, json }) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&cli::tools_json()).unwrap_or_default());
            } else {
                print!("{}", cli::tools_text());
            }
            ExitCode::SUCCESS
        }
        Some(Command::Tools { name: Some(name), json }) => tool_cmd(&name, json),
        Some(Command::Apps { json }) => finish(apps_cmd(json).await),
        Some(Command::State { name, app, json }) => state_cmd(name.as_deref(), app.as_deref(), json).await,
        Some(Command::Prime) => {
            print!("{}", cli::prime_text());
            ExitCode::SUCCESS
        }
        Some(Command::Call(a)) => call_cmd(a).await,
    }
}

fn finish(r: anyhow::Result<()>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Error: {e:?}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(a: ServeArgs) -> anyhow::Result<()> {
    let policy = Policy {
        tools: a.tools.map(names),
        exclude_tools: names(a.exclude_tools),
        allow_launch: a.allow_launch,
        allow_atspi: a.allow_atspi,
    };
    let config = Config {
        retain_snapshots: a.retain_snapshots,
        retain_captures: a.retain_captures,
        retain_exited: a.retain_exited,
        ..Config::default()
    };
    let broker = Broker::new(policy, config).map_err(|e| anyhow::anyhow!(e))?;
    let server = Server::new(broker.clone()).map_err(|e| anyhow::anyhow!(e))?;
    eprintln!("gtk-mcp {} starting (stdio)", env!("CARGO_PKG_VERSION"));
    let service = server.serve(stdio()).await?;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = service.waiting() => {}
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
    // Apps launched by this broker do not outlive it.
    broker.shutdown();
    Ok(())
}

/// A broker for the shell commands.
fn shell_broker(allow_launch: bool, allow_atspi: bool) -> anyhow::Result<Arc<Broker>> {
    let policy = Policy {
        allow_launch,
        allow_atspi,
        ..Policy::default()
    };
    Broker::new(policy, Config::default()).map_err(|e| anyhow::anyhow!(e))
}

async fn doctor_cmd(json: bool) -> ExitCode {
    let config = Config::default();
    let (dir, source) = Config::socket_dir_source();
    let report = doctor::run(&dir, source, config.handshake_timeout).await;
    if json {
        println!("{}", serde_json::to_string_pretty(&report.to_json()).unwrap_or_default());
    } else {
        print!("{}", report.render());
    }
    if report.healthy { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

async fn apps_cmd(json: bool) -> anyhow::Result<()> {
    let broker = shell_broker(false, false)?;
    broker.rescan().await;
    let sessions = broker.sessions();
    if json {
        let list: Vec<Value> = sessions.iter().map(|s| s.listing()).collect();
        println!("{}", serde_json::to_string_pretty(&list)?);
    } else {
        print!("{}", cli::apps_table(&sessions));
    }
    Ok(())
}

fn tool_cmd(name: &str, json: bool) -> ExitCode {
    let defs = match shell_broker(false, false).and_then(|b| Server::new(b).map_err(|e| anyhow::anyhow!(e))) {
        Ok(server) => server.tools(),
        Err(e) => return fail(&AgentError::invalid_params(e.to_string())),
    };
    let out = if json {
        cli::tool_json(&defs, name).map(|v| format!("{}\n", serde_json::to_string_pretty(&v).unwrap_or_default()))
    } else {
        cli::tool_text(&defs, name)
    };
    match out {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}

fn fail(e: &AgentError) -> ExitCode {
    eprintln!("{}", serde_json::to_string_pretty(e).unwrap_or_else(|_| e.message.clone()));
    ExitCode::FAILURE
}

async fn state_cmd(name: Option<&str>, app: Option<&str>, json: bool) -> ExitCode {
    let broker = match shell_broker(false, false) {
        Ok(b) => b,
        Err(e) => return fail(&AgentError::invalid_params(e.to_string())),
    };
    match cli::state(&broker, name, app, json).await {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}

async fn call_cmd(a: CallArgs) -> ExitCode {
    let args = match a.args_json.as_deref() {
        None => None,
        Some(raw) => {
            let mut buf = String::new();
            let text = if raw == "-" {
                if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
                    return fail(&AgentError::invalid_params(format!("cannot read stdin: {e}")));
                }
                buf.as_str()
            } else {
                raw
            };
            match serde_json::from_str::<Value>(text) {
                Ok(v) => Some(v),
                Err(e) => return fail(&AgentError::invalid_params(format!("ARGS_JSON is not valid JSON: {e}"))),
            }
        }
    };
    let broker = match shell_broker(a.allow_launch, a.allow_atspi) {
        Ok(b) => b,
        Err(e) => return fail(&AgentError::invalid_params(e.to_string())),
    };
    match cli::call(&broker, &a.tool, args, a.app.as_deref(), a.output.as_deref()).await {
        Ok(out) => {
            println!("{}", out.stdout);
            for n in out.notes {
                eprintln!("gtk-mcp: {n}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}

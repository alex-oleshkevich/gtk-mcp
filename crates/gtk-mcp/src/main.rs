use clap::Parser;
use gtk_mcp::{Broker, Config, Policy, server::Server};
use rmcp::{ServiceExt, transport::stdio};

/// GTK4 app inspection MCP server (stdio). Logs go to stderr; stdout is MCP only.
#[derive(Parser, Debug)]
#[command(name = "gtk-mcp", version)]
struct Cli {
    /// Comma-separated toolsets to expose.
    #[arg(long, value_delimiter = ',', default_value = "core")]
    toolsets: Vec<String>,
    /// Comma-separated exact tool names; restricts the selected toolsets to these.
    #[arg(long, value_delimiter = ',')]
    tools: Option<Vec<String>>,
    /// Comma-separated exact tool names to hide.
    #[arg(long, value_delimiter = ',', default_value = "")]
    exclude_tools: Vec<String>,
    /// Allow mutating tools (the app must also run with GTK_MCP_MUTATIONS=1).
    #[arg(long)]
    allow_mutations: bool,
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
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let policy = Policy {
        toolsets: names(cli.toolsets),
        tools: cli.tools.map(names),
        exclude_tools: names(cli.exclude_tools),
        allow_mutations: cli.allow_mutations,
        allow_launch: cli.allow_launch,
        allow_atspi: cli.allow_atspi,
    };
    let config = Config {
        retain_snapshots: cli.retain_snapshots,
        retain_captures: cli.retain_captures,
        retain_exited: cli.retain_exited,
        ..Config::default()
    };
    let broker = Broker::new(policy, config).map_err(|e| anyhow::anyhow!(e))?;
    let server = Server::new(broker).map_err(|e| anyhow::anyhow!(e))?;
    eprintln!("gtk-mcp {} starting (stdio)", env!("CARGO_PKG_VERSION"));
    let service = server.serve(stdio()).await?;
    let _ = service.waiting().await;
    Ok(())
}

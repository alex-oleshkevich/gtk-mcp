//! Issue 26: disabled lifecycle tools explain how to configure the MCP server.

mod fixture_env;

use fixture_env::*;
use gtk_mcp::{Broker, Config, Policy};
use gtk_mcp_protocol::ErrorCode;
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn disabled_lifecycle_tools_explain_mcp_server_args_and_broker_restart() {
    let dir = tempfile::tempdir().unwrap();
    let c = Client { broker: Broker::new(Policy::default(), Config::with_socket_dir(dir.path())).unwrap() };
    for (tool, args) in [
        ("launch_app", json!({"argv": ["true"]})),
        ("restart_app", json!({})),
        ("stop_app", json!({})),
    ] {
        let e = c.err(tool, args).await;
        assert_eq!(e.code, ErrorCode::LaunchDisabled, "{tool}: {e}");
        for hint in [tool, "--allow-launch", "gtk-mcp server args", "MCP client config", ".mcp.json args", "restart the broker"] {
            assert!(e.message.contains(hint), "{tool}: missing {hint:?} in {}", e.message);
        }
    }
}

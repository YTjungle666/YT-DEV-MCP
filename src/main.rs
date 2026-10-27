//! Minimal MCP stdio server: newline-delimited JSON-RPC 2.0 over stdin/stdout.
//! No async runtime, no SDK. Three methods are enough for a tools-only server.

use serde_json::{json, Value};
use std::io::{BufRead, Write};

mod plan;
mod tools;
mod util;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--selftest") {
        std::process::exit(util::selftest());
    }
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        // Notifications (no id) never get a response.
        let Some(id) = id else { continue };
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("2024-11-05"),
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "ytdev", "version": env!("CARGO_PKG_VERSION")}
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools::schemas()})),
            "tools/call" => tools::call(&params),
            other => Err((-32601, format!("method not found: {other}"))),
        };
        let msg = match result {
            Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
            Err((code, m)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": m}}),
        };
        if writeln!(stdout, "{msg}").is_err() || stdout.flush().is_err() {
            break;
        }
    }
}

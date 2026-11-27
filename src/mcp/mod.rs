//! Stdio MCP server: newline-delimited JSON-RPC 2.0.
//! stdout carries protocol frames only; everything else goes to stderr.

mod schema;

use crate::tools;
use serde_json::{Value, json};
use std::io::{BufRead, Write};

fn handle(msg: &Value) -> Option<Value> {
    if !msg.is_object() {
        return Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": "invalid request"}}));
    }
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        return msg.get("id").map(|id| {
            json!({"jsonrpc": "2.0", "id": id.clone(), "error": {"code": -32600, "message": "invalid request"}})
        });
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2024-11-05"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "agyfast-rs", "version": env!("CARGO_PKG_VERSION")},
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": schema::tools()})),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default().to_string();
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            // A panicking tool must not take the whole server down.
            let outcome = std::panic::catch_unwind(|| tools::call(&name, args))
                .unwrap_or_else(|_| Err(format!("Error: {name} crashed internally; fall back to built-in tools for this call")));
            let (text, is_error) = match outcome {
                Ok(text) => (text, false),
                Err(text) => (text, true),
            };
            Ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error}))
        }
        _ => Err(json!({"code": -32601, "message": format!("method not found: {method}")})),
    };
    // Notifications carry no id and get no reply.
    let id = msg.get("id")?.clone();
    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": id, "error": error}),
    })
}

pub fn serve() -> i32 {
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg),
            Err(e) => Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}})),
        };
        if let Some(reply) = reply
            && writeln!(out, "{reply}").and_then(|_| out.flush()).is_err()
        {
            break;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_basics() {
        let init = handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}})).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert!(handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
        let list = handle(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["batch_search", "quick_outline", "apply_edits", "run_checks"]);
        // read_slices is unlisted but still answers a host that calls it.
        let slices = handle(&json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "read_slices", "arguments": {"lineRanges": ["Cargo.toml:1-2"]}}})).unwrap();
        assert_eq!(slices["result"]["isError"], false, "{slices}");
        assert!(slices["result"]["content"][0]["text"].as_str().unwrap().contains("[package]"), "{slices}");
        let unknown = handle(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "nope", "arguments": {"toolSummary": "x"}}})).unwrap();
        assert_eq!(unknown["result"]["isError"], true);
        assert_eq!(handle(&json!({"jsonrpc": "2.0", "id": 4, "method": "bogus"})).unwrap()["error"]["code"], -32601);
        assert_eq!(handle(&json!([1])).unwrap()["error"]["code"], -32600);
        assert_eq!(handle(&json!({"jsonrpc": "2.0", "id": 1})).unwrap()["error"]["code"], -32600);
        assert_eq!(handle(&json!({"jsonrpc": "2.0", "id": 1, "method": 123})).unwrap()["error"]["code"], -32600);
        assert!(handle(&json!({"jsonrpc": "2.0"})).is_none());
    }
}

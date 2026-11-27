use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
fn mcp_session_over_stdio() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agyfast-rs"))
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let frames = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "quick_outline", "arguments": {"paths": ["Cargo.toml"], "toolSummary": "outline"}}}),
    ];
    let mut stdin = child.stdin.take().unwrap();
    for f in &frames {
        writeln!(stdin, "{f}").unwrap();
    }
    drop(stdin); // closing stdin must end the server

    let replies: Vec<Value> = BufReader::new(child.stdout.take().unwrap())
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).expect("stdout line is JSON"))
        .collect();
    assert!(child.wait().unwrap().success());

    assert!(replies.iter().all(|r| r["jsonrpc"] == "2.0"));
    let ids: Vec<_> = replies.iter().map(|r| r["id"].clone()).collect();
    assert_eq!(ids, [json!(1), json!(2), json!(3)], "the notification must get no reply");
    assert!(replies[0]["result"]["capabilities"]["tools"].is_object());
    let tools = replies[1]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 4);
    for name in ["search", "outline", "edit", "check"] {
        assert!(tools.iter().any(|t| t["name"] == name), "missing {name}");
    }
    // toolSummary must not make the call a protocol error.
    assert!(replies[2].get("error").is_none() && replies[2]["result"]["content"].is_array());
}

#[test]
fn a_non_utf8_frame_gets_a_parse_error_and_the_server_lives_on() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agyfast-rs")).arg("mcp").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"\xff\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n").unwrap();
    let out = child.wait_with_output().unwrap();
    let replies: Vec<Value> = String::from_utf8(out.stdout).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!((replies[0]["error"]["code"].clone(), replies[1]["id"].clone()), (json!(-32700), json!(1)), "{replies:?}");
}

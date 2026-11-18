//! `pdfcraft-cli mcp --compact`: the flag reaches the server, `--root` is still honoured, and plain `mcp` still lists every tool.
#![cfg(feature = "mcp")]

use std::io::Write as _;
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// Run `pdfcraft-cli mcp <flags>` over an initialize request plus `requests` and return the replies to `requests`.
fn session(flags: &[&str], requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pdfcraft-cli"))
        .arg("mcp")
        .args(flags)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = String::from("{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"initialize\",\"params\":{}}\n");
    for r in requests {
        input.push_str(&r.to_string());
        input.push('\n');
    }
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().lines().skip(1).map(|l| serde_json::from_str(l).unwrap()).collect()
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// The `tools/list` tool names of `pdfcraft-cli mcp <flags>`.
fn listed_tools(flags: &[&str]) -> Vec<String> {
    let reply = session(flags, &[request(1, "tools/list", json!({}))]).remove(0);
    reply["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
}

#[cfg(target_os = "macos")]
#[test]
fn ai_mcp_lists_the_tools_and_discovers_models_in_compact_mode() {
    use std::net::TcpListener;

    use super::text::{ai_mock_reply, ai_mock_request};

    let full = session(&[], &[request(1, "tools/list", json!({}))]).remove(0);
    let tools = full["result"]["tools"].as_array().unwrap();
    for name in ["ai_ask", "ai_summarize", "ai_translate", "ai_llama_models"] {
        let tool = tools.iter().find(|tool| tool["name"] == name).unwrap_or_else(|| panic!("Missing MCP tool {name}"));
        assert_eq!(tool["annotations"]["openWorldHint"], true, "{name}");
        if name != "ai_llama_models" {
            let fields = &tool["inputSchema"]["properties"];
            assert_eq!(fields["user_confirmed"]["type"], "boolean", "{name}");
            assert_eq!(fields["confirmed_endpoint"]["type"], "string", "{name}");
            assert_eq!(fields["confirmed_model"]["type"], "string", "{name}");
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    std::thread::scope(|scope| {
        let server = scope.spawn(|| {
            let (socket, body) = ai_mock_request(&listener, "GET /v1/models");
            assert!(body.is_empty());
            ai_mock_reply(socket, r#"{"data":[{"id":"local-model"}]}"#);
        });
        let call = request(2, "tools/call", json!({"name":"ai_llama_models","arguments":{"base_url":base_url}}));
        let reply = session(&["--compact"], &[call]).remove(0);
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        assert_eq!(reply["result"]["structuredContent"], json!({"models":["local-model"]}));
        server.join().unwrap();
    });
}

#[test]
fn mcp_rejects_unknown_options_before_starting_the_server() {
    for option in ["--totally-bogus-flag", "--rot", "--root=."] {
        let out = Command::new(env!("CARGO_BIN_EXE_pdfcraft-cli")).args(["mcp", option]).output().expect("run CLI");

        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "mcp {option} unexpectedly succeeded");
        assert!(stderr.contains("unknown option") && stderr.contains(option), "mcp {option}: {stderr}");
        assert!(!stderr.contains("MCP server on stdio"), "server started despite invalid option: {stderr}");
    }
}

#[test]
fn compact_flag_shortens_the_tool_list() {
    let full = listed_tools(&[]);
    let compact = listed_tools(&["--compact"]);
    assert_eq!(compact.len(), 17, "{compact:?}");
    assert!(compact.iter().any(|n| n == "tool_search") && compact.iter().any(|n| n == "tool_call"));
    assert!(compact.iter().all(|n| full.contains(n) || n == "tool_search" || n == "tool_call"));
    assert!(full.len() > 100 && !full.iter().any(|n| n == "tool_search"), "{} tools", full.len());
    // The flag can sit on either side of --root.
    assert_eq!(listed_tools(&["--root", ".", "--compact"]), compact);
    assert_eq!(listed_tools(&["--compact", "--root", "."]), compact);
}

#[test]
fn compact_flag_keeps_the_root() {
    let root = std::env::temp_dir().join(format!("pdfcraft-cli-compact-root-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let outside = std::env::temp_dir().join(format!("pdfcraft-cli-compact-outside-{}.pdf", std::process::id()));
    let outside = outside.to_string_lossy().into_owned();
    let root_arg = root.to_string_lossy().into_owned();
    let open_outside =
        request(1, "tools/call", json!({ "name": "tool_call", "arguments": { "name": "doc_open", "arguments": { "path": outside } } }));
    let text = |reply: &Value| reply["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string();

    // With the root, in either flag order, a path outside it is refused with the root's own message.
    for flags in [vec!["--root", root_arg.as_str(), "--compact"], vec!["--compact", "--root", root_arg.as_str()]] {
        let reply = session(&flags, std::slice::from_ref(&open_outside)).remove(0);
        assert_eq!(reply["result"]["isError"], true, "{flags:?}: {reply}");
        assert!(text(&reply).contains("is outside the allowed directory"), "{flags:?}: {}", text(&reply));
    }
    // Without a root the same call gets past that check (the file does not exist, so it fails for that reason instead).
    let reply = session(&["--compact"], std::slice::from_ref(&open_outside)).remove(0);
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    assert!(!text(&reply).contains("is outside the allowed directory"), "{}", text(&reply));
    let _ = std::fs::remove_dir_all(&root);
}

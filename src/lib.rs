//! OpenCode AI Agent plugin for Basalt.
//!
//! Provides `CAP_AGENT_LAUNCHER` for running OpenCode agent sessions with dynamic model
//! discovery (`opencode models`) and effort variants (`low`, `medium`, `high`, `max`).

use basalt_plugin_sdk::prelude::*;
use basalt_host_shims as _;

pub const PLUGIN_NAME: &str = "opencode";
pub const PLUGIN_VERSION: &str = "0.1.0";

basalt_plugin_meta! {
    name:         "opencode",
    version:      "0.1.0",
    hook_flags:   CAP_AGENT_LAUNCHER,
    provides:     "agent-launcher@opencode/v1",
    requires:     "",
    optional_requires: "",
    file_globs:   "",
    activates_on: "",
    activation_events: "",
}

#[no_mangle]
pub extern "C" fn basalt_agent_metadata() -> u64 {
    let meta = AgentMetadata {
        name: "OpenCode AI Agent".into(),
        executable: "opencode".into(),
        args: vec!["run".into(), "--format".into(), "json".into(), "--thinking".into(), "--auto".into(), "{prompt}".into()],
        resume_new_args: vec!["run".into(), "--format".into(), "json".into(), "--thinking".into(), "--auto".into(), "{prompt}".into()],
        resume_cont_args: vec!["run".into(), "--format".into(), "json".into(), "--thinking".into(), "--auto".into(), "--session".into(), "{session_id}".into(), "{prompt}".into()],
        execution_tier: AgentExecutionTier::StructuredDirect,
        workspace_capabilities: vec!["mcp".into(), "shadow".into()],
        protocol: AgentProtocol::Cli,
    };
    let bytes = encode_agent_metadata(&meta);
    pack_output(bytes)
}

#[no_mangle]
pub extern "C" fn basalt_agent_settings_schema() -> u64 {
    let schema = serde_json::json!({
        "plugin": "opencode",
        "dynamic_models": true,
        "models_command": "opencode models",
        "variants": [
            "default",
            "low",
            "medium",
            "high",
            "max"
        ],
        "default_variant": "default"
    });
    let bytes = serde_json::to_vec(&schema).unwrap_or_default();
    pack_output(bytes)
}

/// Pure implementation of OpenCode launch preparation for testability and guest execution.
pub fn prepare_opencode_launch(req: &AgentLaunchRequest) -> AgentLaunchPreparation {
    let mut permissions = serde_json::Map::new();
    for tool in &req.disabled_tools {
        match tool {
            StandardTool::Read => {
                permissions.insert("read".to_string(), serde_json::Value::String("deny".to_string()));
            }
            StandardTool::Write => {
                permissions.insert("edit".to_string(), serde_json::Value::String("deny".to_string()));
                permissions.insert("write".to_string(), serde_json::Value::String("deny".to_string()));
            }
            StandardTool::Execute => {
                permissions.insert("bash".to_string(), serde_json::Value::String("deny".to_string()));
            }
            StandardTool::Question => {
                permissions.insert("question".to_string(), serde_json::Value::String("deny".to_string()));
            }
        }
    }

    let mut config_json = serde_json::json!({
        "$schema": "https://opencode.ai/config.json"
    });

    if let Some(ref url) = req.mcp_url {
        config_json["mcp"] = serde_json::json!({
            "basalt": {
                "type": "remote",
                "url": url
            }
        });
    }

    if !permissions.is_empty() {
        config_json["permission"] = serde_json::Value::Object(permissions);
    }

    AgentLaunchPreparation {
        extra_args: Vec::new(),
        env: std::collections::HashMap::new(),
        workspace_files: vec![AgentWorkspaceFile {
            relative_path: ".opencode/opencode.json".to_string(),
            content: serde_json::to_string_pretty(&config_json).unwrap_or_default(),
        }],
    }
}

#[no_mangle]
pub extern "C" fn basalt_agent_prepare_launch(
    req_ptr: *const u8,
    req_len: u32,
) -> u64 {
    let req: AgentLaunchRequest = if !req_ptr.is_null() && req_len > 0 {
        let slice = unsafe { std::slice::from_raw_parts(req_ptr, req_len as usize) };
        serde_json::from_slice(slice).unwrap_or_default()
    } else {
        AgentLaunchRequest::default()
    };

    let prep = prepare_opencode_launch(&req);
    let bytes = serde_json::to_vec(&prep).unwrap_or_default();
    pack_output(bytes)
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if let Some(&next) = chars.peek() {
                if next == '[' {
                    chars.next(); // consume '['
                    // consume parameters and final byte
                    while let Some(&ch) = chars.peek() {
                        chars.next();
                        if ('\x40'..='\x7e').contains(&ch) {
                            break;
                        }
                    }
                } else if next == ']' {
                    chars.next(); // consume ']'
                    while let Some(&ch) = chars.peek() {
                        chars.next();
                        if ch == '\x07' {
                            break;
                        }
                        if ch == '\x1b' {
                            if let Some(&'\\') = chars.peek() {
                                chars.next();
                            }
                            break;
                        }
                    }
                } else if next == '(' || next == ')' {
                    chars.next();
                    chars.next();
                } else {
                    chars.next();
                }
            }
        } else if !c.is_control() || c == '\n' || c == '\t' {
            out.push(c);
        }
    }
    out
}

fn extract_opencode_error(val: &serde_json::Value, part_obj: Option<&serde_json::Value>) -> String {
    let candidate = part_obj
        .and_then(|p| p.get("error").or_else(|| p.get("message")))
        .or_else(|| val.get("error").or_else(|| val.get("message")));

    if let Some(err_val) = candidate {
        if let Some(s) = err_val.as_str() {
            return s.to_string();
        }
        if let Some(msg) = err_val.get("data").and_then(|d| d.get("message")).and_then(|m| m.as_str()) {
            return msg.to_string();
        }
        if let Some(msg) = err_val.get("message").and_then(|m| m.as_str()) {
            return msg.to_string();
        }
        if let Some(name) = err_val.get("name").and_then(|n| n.as_str()) {
            return name.to_string();
        }
        return err_val.to_string();
    }
    "Unknown error".to_string()
}

pub fn parse_opencode_json_line(line_str: &str) -> Vec<AgentEvent> {
    let mut events = Vec::new();

    if let Ok(val) = serde_json::from_str::<serde_json::Value>(line_str) {
        let part_obj = val.get("part");
        let event_type = val.get("type")
            .and_then(|t| t.as_str())
            .unwrap_or_else(|| {
                part_obj.and_then(|p| p.get("type")).and_then(|t| t.as_str()).unwrap_or("")
            });

        // 1. Session start / ID availability
        if event_type == "step_start" || event_type == "step-start"
            || part_obj.and_then(|p| p.get("type")).and_then(|t| t.as_str()) == Some("step-start")
        {
            if let Some(sid) = val.get("sessionID").or_else(|| val.get("sessionId")).or_else(|| val.get("session_id")).and_then(|s| s.as_str()) {
                events.push(AgentEvent::SessionIDAvailable(sid.to_string()));
            }
            return events;
        }

        // 2. Tool Call / Tool Use
        if event_type == "tool_call" || event_type == "tool_use" || event_type == "call"
            || part_obj.and_then(|p| p.get("type")).and_then(|t| t.as_str()) == Some("tool")
        {
            let tool_name = part_obj
                .and_then(|p| p.get("tool").or_else(|| p.get("name")))
                .or_else(|| val.get("tool").or_else(|| val.get("name")))
                .and_then(|t| t.as_str())
                .unwrap_or("tool");

            let call_id = part_obj
                .and_then(|p| p.get("callID").or_else(|| p.get("id")))
                .or_else(|| val.get("id").or_else(|| val.get("call_id")))
                .and_then(|i| i.as_str())
                .unwrap_or("call");

            let lower = tool_name.to_lowercase();
            let category = if lower.contains("read") || lower.contains("view") {
                "read"
            } else if lower.contains("write") || lower.contains("edit") || lower.contains("replace") {
                "write"
            } else if lower.contains("bash") || lower.contains("run") || lower.contains("exec") {
                "run"
            } else if lower.contains("ask") || lower.contains("question") {
                "question"
            } else {
                "run"
            };

            let state_obj = part_obj.and_then(|p| p.get("state"));
            let input_val = state_obj
                .and_then(|s| s.get("input"))
                .or_else(|| part_obj.and_then(|p| p.get("input").or_else(|| p.get("args"))))
                .or_else(|| val.get("input").or_else(|| val.get("args")));

            let raw_cmd = input_val.map(|a| a.to_string()).unwrap_or_default();
            let mut file_paths = Vec::new();
            if let Some(args) = input_val {
                if let Some(path) = args.get("filePath")
                    .or_else(|| args.get("path"))
                    .or_else(|| args.get("file_path"))
                    .or_else(|| args.get("file"))
                    .and_then(|p| p.as_str())
                {
                    file_paths.push(path.to_string());
                }
            }

            let output_str = state_obj
                .and_then(|s| s.get("output"))
                .or_else(|| part_obj.and_then(|p| p.get("output").or_else(|| p.get("result"))))
                .or_else(|| val.get("output").or_else(|| val.get("result")))
                .and_then(|o| o.as_str());

            events.push(AgentEvent::NewEntry {
                vendor_id: call_id.to_string(),
                tool: tool_name.to_string(),
                category: category.to_string(),
                raw_cmd,
                file_paths,
            });

            if let Some(output) = output_str {
                let lines: Vec<String> = output.lines().map(|l| strip_ansi(l)).collect();
                events.push(AgentEvent::CloseEntry {
                    vendor_id: call_id.to_string(),
                    exit_code: 0,
                    output_lines: lines,
                });
            }
        } else if event_type == "tool_result" || event_type == "result" {
            let call_id = part_obj
                .and_then(|p| p.get("callID").or_else(|| p.get("id")))
                .or_else(|| val.get("id").or_else(|| val.get("call_id")))
                .and_then(|i| i.as_str())
                .unwrap_or("call");
            let output = part_obj
                .and_then(|p| p.get("output").or_else(|| p.get("result")))
                .or_else(|| val.get("output").or_else(|| val.get("result")))
                .and_then(|o| o.as_str())
                .unwrap_or("");
            let exit_code = val.get("exit_code").and_then(|c| c.as_i64()).unwrap_or(0) as i32;
            let lines: Vec<String> = output.lines().map(|l| strip_ansi(l)).collect();
            events.push(AgentEvent::CloseEntry {
                vendor_id: call_id.to_string(),
                exit_code,
                output_lines: lines,
            });
        } else if event_type == "question" || event_type == "ask" {
            let q_id = val.get("id").and_then(|i| i.as_str()).unwrap_or("question");
            let text = val.get("text").or_else(|| val.get("question")).or_else(|| val.get("message")).and_then(|t| t.as_str()).unwrap_or("");
            events.push(AgentEvent::NewEntry {
                vendor_id: q_id.to_string(),
                tool: format!("Question: {}", strip_ansi(text)),
                category: "question".into(),
                raw_cmd: text.to_string(),
                file_paths: Vec::new(),
            });
        } else if event_type == "reasoning" || event_type == "thought" || event_type == "thinking"
            || part_obj.and_then(|p| p.get("type")).and_then(|t| t.as_str()) == Some("reasoning")
            || part_obj.and_then(|p| p.get("type")).and_then(|t| t.as_str()) == Some("thought")
        {
            let text = part_obj
                .and_then(|p| p.get("text").or_else(|| p.get("thought")).or_else(|| p.get("reasoning")))
                .or_else(|| val.get("text").or_else(|| val.get("thought")).or_else(|| val.get("reasoning")))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            let cleaned = strip_ansi(text);
            if !cleaned.trim().is_empty() {
                events.push(AgentEvent::NewEntry {
                    vendor_id: format!("thought-{}", cleaned.len()),
                    tool: cleaned.chars().take(80).collect(),
                    category: "thought".into(),
                    raw_cmd: cleaned,
                    file_paths: Vec::new(),
                });
            }
        } else if event_type == "text" || event_type == "message" || event_type == "content"
            || part_obj.and_then(|p| p.get("type")).and_then(|t| t.as_str()) == Some("text")
        {
            let text = part_obj
                .and_then(|p| p.get("text").or_else(|| p.get("content")).or_else(|| p.get("message")))
                .or_else(|| val.get("text").or_else(|| val.get("content")).or_else(|| val.get("message")))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            let cleaned = strip_ansi(text);
            if !cleaned.trim().is_empty() {
                events.push(AgentEvent::NewEntry {
                    vendor_id: format!("msg-{}", cleaned.len()),
                    tool: cleaned.chars().take(80).collect(),
                    category: "message".into(),
                    raw_cmd: cleaned,
                    file_paths: Vec::new(),
                });
            }
        } else if event_type == "done" || event_type == "complete" || event_type == "finish"
            || event_type == "step_finish" || event_type == "step-finish"
            || part_obj.and_then(|p| p.get("type")).and_then(|t| t.as_str()) == Some("step-finish")
        {
            let reason = part_obj
                .and_then(|p| p.get("reason"))
                .or_else(|| val.get("reason"))
                .and_then(|r| r.as_str())
                .unwrap_or("stop");
            let is_success = reason != "error" && reason != "cancelled" && reason != "failed";
            let error = if !is_success {
                let err_msg = extract_opencode_error(&val, part_obj);
                if err_msg != "Unknown error" {
                    Some(err_msg)
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(ref err) = error {
                let cleaned = strip_ansi(err);
                events.push(AgentEvent::NewEntry {
                    vendor_id: format!("err-{}", cleaned.len()),
                    tool: cleaned.chars().take(80).collect(),
                    category: "message".into(),
                    raw_cmd: cleaned.clone(),
                    file_paths: Vec::new(),
                });
            }
            events.push(AgentEvent::SessionEnded { success: is_success, error });
        } else if event_type == "error" {
            let err_msg = extract_opencode_error(&val, part_obj);
            let cleaned = strip_ansi(&err_msg);
            events.push(AgentEvent::NewEntry {
                vendor_id: format!("err-{}", cleaned.len()),
                tool: cleaned.chars().take(80).collect(),
                category: "message".into(),
                raw_cmd: cleaned.clone(),
                file_paths: Vec::new(),
            });
            events.push(AgentEvent::SessionEnded {
                success: false,
                error: Some(cleaned),
            });
        } else {
            // Fallback for general text/message
            if let Some(text) = part_obj.and_then(|p| p.get("text")).or_else(|| val.get("text")).or_else(|| val.get("message")).or_else(|| val.get("data")).and_then(|t| t.as_str()) {
                let cleaned = strip_ansi(text);
                if !cleaned.trim().is_empty() {
                    events.push(AgentEvent::NewEntry {
                        vendor_id: format!("data-{}", cleaned.len()),
                        tool: cleaned.chars().take(80).collect(),
                        category: "log".into(),
                        raw_cmd: cleaned,
                        file_paths: Vec::new(),
                    });
                }
            }
        }
    } else {
        // Plain text fallback with ANSI stripped
        let cleaned = strip_ansi(line_str);
        if !cleaned.trim().is_empty() {
            let category = if cleaned.to_lowercase().contains("permission requested") {
                "question"
            } else if cleaned.to_lowercase().contains("error") {
                "diagnostic"
            } else {
                "log"
            };
            events.push(AgentEvent::NewEntry {
                vendor_id: format!("raw-{}", cleaned.len()),
                tool: cleaned.chars().take(80).collect(),
                category: category.into(),
                raw_cmd: cleaned,
                file_paths: Vec::new(),
            });
        }
    }

    events
}

#[no_mangle]
pub extern "C" fn basalt_agent_parse_line(
    line_ptr: *const u8,
    line_len: u32,
    _state_ptr: *const u8,
    _state_len: u32,
) -> u64 {
    if line_ptr.is_null() || line_len == 0 {
        return pack_output(encode_agent_parse_output(&[], &[]));
    }
    let line_slice = unsafe { std::slice::from_raw_parts(line_ptr, line_len as usize) };
    let line_str = match std::str::from_utf8(line_slice) {
        Ok(s) => s.trim(),
        Err(_) => return pack_output(encode_agent_parse_output(&[], &[])),
    };

    if line_str.is_empty() {
        return pack_output(encode_agent_parse_output(&[], &[]));
    }

    let events = parse_opencode_json_line(line_str);
    pack_output(encode_agent_parse_output(&[], &events))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_opencode_stream_events() {
        // 1. step_start with sessionID
        let step_start_json = r#"{"type":"step_start","timestamp":1789240423264,"sessionID":"ses_f68f5f178ffeDT2AycolEs49NT","part":{"id":"prt_0970a333f001oxLDbvE24xsznE","messageID":"msg_0970a1bc6001hi1gQHwzKhRJx8","sessionID":"ses_f68f5f178ffeDT2AycolEs49NT","type":"step-start"}}"#;
        let evs = parse_opencode_json_line(step_start_json);
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            AgentEvent::SessionIDAvailable(id) => assert_eq!(id, "ses_f68f5f178ffeDT2AycolEs49NT"),
            _ => panic!("expected SessionIDAvailable"),
        }

        // 2. reasoning / thinking block
        let reasoning_json = r#"{"type":"reasoning","timestamp":1789240423264,"sessionID":"ses_f68f5f178ffeDT2AycolEs49NT","part":{"id":"prt_0970a3342001CjI6CC4irIXYen","messageID":"msg_0970a1bc6001hi1gQHwzKhRJx8","sessionID":"ses_f68f5f178ffeDT2AycolEs49NT","type":"reasoning","text":"The user sent a simple test message asking me to reply with \"OK\". No tools needed, just a direct response.","time":{"start":1789240423234,"end":1789240423244}}}"#;
        let evs = parse_opencode_json_line(reasoning_json);
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            AgentEvent::NewEntry { category, raw_cmd, .. } => {
                assert_eq!(category, "thought");
                assert!(raw_cmd.contains("reply with \"OK\""));
            }
            _ => panic!("expected NewEntry with thought"),
        }

        // 3. text / message
        let text_json = r#"{"type":"text","timestamp":1789240423264,"sessionID":"ses_f68f5f178ffeDT2AycolEs49NT","part":{"id":"prt_0970a334e001LmsYB1D3NN4aEu","messageID":"msg_0970a1bc6001hi1gQHwzKhRJx8","sessionID":"ses_f68f5f178ffeDT2AycolEs49NT","type":"text","text":"OK","time":{"start":1789240423246,"end":1789240423248}}}"#;
        let evs = parse_opencode_json_line(text_json);
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            AgentEvent::NewEntry { category, raw_cmd, .. } => {
                assert_eq!(category, "message");
                assert_eq!(raw_cmd, "OK");
            }
            _ => panic!("expected NewEntry with message"),
        }

        // 4. step_finish (stop)
        let step_finish_json = r#"{"type":"step_finish","timestamp":1789240423264,"sessionID":"ses_f68f5f178ffeDT2AycolEs49NT","part":{"id":"prt_0970a3353001THyTdtCclTi8h0","reason":"stop","messageID":"msg_0970a1bc6001hi1gQHwzKhRJx8","sessionID":"ses_f68f5f178ffeDT2AycolEs49NT","type":"step-finish","tokens":{"total":7256,"input":7229,"output":3,"reasoning":24,"cache":{"write":0,"read":0}},"cost":0}}"#;
        let evs = parse_opencode_json_line(step_finish_json);
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            AgentEvent::SessionEnded { success, error: _ } => assert!(success),
            _ => panic!("expected SessionEnded"),
        }

        // 5. completed tool call
        let tool_json = r#"{"type":"tool_use","part":{"type":"tool","tool":"read","callID":"c1","state":{"status":"completed","input":{"filePath":"src/main.rs"},"output":"fn main() {}"}}}"#;
        let evs = parse_opencode_json_line(tool_json);
        assert_eq!(evs.len(), 2);
        match &evs[0] {
            AgentEvent::NewEntry { tool, category, file_paths, .. } => {
                assert_eq!(tool, "read");
                assert_eq!(category, "read");
                assert_eq!(file_paths, &vec!["src/main.rs".to_string()]);
            }
            _ => panic!("expected NewEntry for tool"),
        }
        match &evs[1] {
            AgentEvent::CloseEntry { vendor_id, exit_code, output_lines } => {
                assert_eq!(vendor_id, "c1");
                assert_eq!(*exit_code, 0);
                assert_eq!(output_lines, &vec!["fn main() {}".to_string()]);
            }
            _ => panic!("expected CloseEntry for tool"),
        }
    }

    #[test]
    fn test_prepare_opencode_launch_permissions() {
        let req = AgentLaunchRequest {
            mcp_url: Some("http://127.0.0.1:9999".into()),
            disabled_tools: vec![StandardTool::Read, StandardTool::Write],
            model: None,
            variant: None,
        };

        let prep = prepare_opencode_launch(&req);
        assert_eq!(prep.workspace_files.len(), 1);
        assert_eq!(prep.workspace_files[0].relative_path, ".opencode/opencode.json");

        let json_val: serde_json::Value = serde_json::from_str(&prep.workspace_files[0].content).unwrap();
        assert_eq!(json_val["mcp"]["basalt"]["url"], "http://127.0.0.1:9999");
        assert_eq!(json_val["permission"]["read"], "deny");
        assert_eq!(json_val["permission"]["edit"], "deny");
        assert_eq!(json_val["permission"]["write"], "deny");
    }

    #[test]
    fn test_prepare_opencode_launch_no_disabled_tools() {
        let req = AgentLaunchRequest {
            mcp_url: Some("http://127.0.0.1:9999".into()),
            disabled_tools: Vec::new(),
            model: None,
            variant: None,
        };

        let prep = prepare_opencode_launch(&req);
        assert_eq!(prep.workspace_files.len(), 1);
        let json_val: serde_json::Value = serde_json::from_str(&prep.workspace_files[0].content).unwrap();
        assert_eq!(json_val["mcp"]["basalt"]["url"], "http://127.0.0.1:9999");
        assert!(json_val.get("permission").is_none());
    }

    #[test]
    fn test_parse_basalt_tools_categorization() {
        let read_json = r#"{"type":"tool_use","part":{"type":"tool","tool":"basalt_read_file","callID":"c2","state":{"status":"completed","input":{"path":"Cargo.toml"}}}}"#;
        let evs = parse_opencode_json_line(read_json);
        match &evs[0] {
            AgentEvent::NewEntry { tool, category, .. } => {
                assert_eq!(tool, "basalt_read_file");
                assert_eq!(category, "read");
            }
            _ => panic!("expected NewEntry for basalt_read_file"),
        }

        let write_json = r#"{"type":"tool_use","part":{"type":"tool","tool":"basalt_write_file","callID":"c3","state":{"status":"completed","input":{"path":"Cargo.toml","content":""}}}}"#;
        let evs = parse_opencode_json_line(write_json);
        match &evs[0] {
            AgentEvent::NewEntry { tool, category, .. } => {
                assert_eq!(tool, "basalt_write_file");
                assert_eq!(category, "write");
            }
            _ => panic!("expected NewEntry for basalt_write_file"),
        }
    }

    #[test]
    fn test_parse_opencode_error_event() {
        let error_json = r#"{"type":"error","timestamp":1789811125144,"sessionID":"ses_f46f1a177ffeCfJ0PC2BN32wdK","error":{"name":"APIError","data":{"message":"Error from provider (Console): OpenCode's free tier can only be used from within OpenCode","statusCode":403}}}"#;
        let evs = parse_opencode_json_line(error_json);
        assert_eq!(evs.len(), 2);
        match &evs[0] {
            AgentEvent::NewEntry { category, raw_cmd, .. } => {
                assert_eq!(category, "message");
                assert!(raw_cmd.contains("OpenCode's free tier can only be used from within OpenCode"));
            }
            _ => panic!("expected NewEntry with error message"),
        }
        match &evs[1] {
            AgentEvent::SessionEnded { success, error } => {
                assert!(!success);
                assert!(error.as_ref().unwrap().contains("OpenCode's free tier can only be used from within OpenCode"));
            }
            _ => panic!("expected SessionEnded with error"),
        }
    }
}

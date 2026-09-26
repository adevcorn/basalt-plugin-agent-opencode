//! OpenCode AI Agent plugin for Basalt.
//!
//! Provides `CAP_AGENT_LAUNCHER` for running OpenCode agent sessions with dynamic model
//! discovery (`opencode models`) and effort variants (`low`, `medium`, `high`, `max`).

use basalt_plugin_sdk::prelude::*;

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

/// Launch-contract types shared with the Basalt host as JSON.
///
/// These mirror `basalt-core/src/agent_metadata.rs`. They intentionally live
/// here (rather than in `basalt-plugin-sdk`, which no longer exports them) so
/// the plugin stays self-contained and buildable against the current SDK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StandardTool {
    Read,
    Write,
    Execute,
    Question,
}

/// A single file to materialize into the agent's workspace before launch.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentWorkspaceFile {
    pub relative_path: String,
    pub content: String,
}

/// Request passed (as JSON) to `basalt_agent_prepare_launch` by the host.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentLaunchRequest {
    #[serde(default)]
    pub mcp_url: Option<String>,
    #[serde(default)]
    pub disabled_tools: Vec<StandardTool>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub variant: Option<String>,
    #[serde(default)]
    pub workspace_path: Option<String>,
}

/// Result of launch preparation: extra CLI args, env vars, and workspace files.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AgentLaunchPreparation {
    #[serde(default)]
    pub extra_args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub workspace_files: Vec<AgentWorkspaceFile>,
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
        // Declares the shadow config this agent needs; the host renders
        // `.opencode/opencode.json` from the session MCP URL (plugin-wins
        // on collision, so the hand-rolled file below keeps priority).
        workspace_capabilities: vec!["mcp".into(), "shadow".into(), "config:opencode".into()],
        protocol: AgentProtocol::Cli,
    };
    let bytes = encode_agent_metadata(&meta);
    pack_output(bytes)
}

#[no_mangle]
pub extern "C" fn basalt_agent_settings_schema() -> u64 {
    // The host decodes this export with the binary `encode_agent_settings_schema`
    // wire format (see `basalt-plugin-sdk`), not JSON. Model discovery is done
    // host-side via `fetch_agent_models` (`opencode models`), so no fields are
    // needed here.
    pack_output(encode_agent_settings_schema(&[]))
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

/// State byte flags threaded through `basalt_agent_parse_line`.
const STATE_NONE: u8 = 0;
const STATE_MSG_OPEN: u8 = 1;
const STATE_THOUGHT_OPEN: u8 = 2;

static OPEN_TOOLS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Stateful parse: processes one JSON line and returns `(new_state, events)`.
pub fn parse_opencode_line_stateful(line_str: &str, open_entry: u8) -> (u8, Vec<AgentEvent>) {
    let mut events = Vec::new();

    if let Ok(val) = serde_json::from_str::<serde_json::Value>(line_str) {
        let part_obj = val.get("part").or_else(|| val.get("data"));
        let event_type = val.get("type")
            .and_then(|t| t.as_str())
            .unwrap_or_else(|| {
                part_obj.and_then(|p| p.get("type")).and_then(|t| t.as_str()).unwrap_or("")
            });

        // 1. Session start / ID availability
        if event_type == "step_start" || event_type == "step-start" || event_type == "session_start" || event_type == "session-start"
            || part_obj.and_then(|p| p.get("type")).and_then(|t| t.as_str()) == Some("step-start")
        {
            if let Ok(mut set) = OPEN_TOOLS.lock() {
                set.clear();
            }
            if let Some(sid) = val.get("sessionID")
                .or_else(|| val.get("sessionId"))
                .or_else(|| val.get("session_id"))
                .or_else(|| part_obj.and_then(|p| p.get("sessionID").or_else(|| p.get("sessionId")).or_else(|| p.get("session_id"))))
                .and_then(|s| s.as_str())
            {
                events.push(AgentEvent::SessionIDAvailable(sid.to_string()));
            }
            return (STATE_NONE, events);
        }

        // 2. Tool Call / Tool Use
        if event_type == "tool_call" || event_type == "tool_use" || event_type == "call"
            || part_obj.and_then(|p| p.get("type")).and_then(|t| t.as_str()) == Some("tool")
        {
            let raw_tool_name = part_obj
                .and_then(|p| p.get("tool").or_else(|| p.get("name")).or_else(|| p.get("tool_name")))
                .or_else(|| val.get("tool").or_else(|| val.get("name")).or_else(|| val.get("tool_name")))
                .and_then(|t| t.as_str());

            // If neither part nor top-level specifies a tool name or part type, this is a generic container header object (e.g. {"type":"tool_use", "sessionID":"..."}).
            if raw_tool_name.is_none() && part_obj.is_none() {
                if let Some(sid) = val.get("sessionID").or_else(|| val.get("sessionId")).or_else(|| val.get("session_id")).and_then(|s| s.as_str()) {
                    events.push(AgentEvent::SessionIDAvailable(sid.to_string()));
                }
                return (STATE_NONE, events);
            }

            let tool_name = raw_tool_name.unwrap_or("tool");

            let call_id = part_obj
                .and_then(|p| p.get("callID").or_else(|| p.get("id")).or_else(|| p.get("call_id")))
                .or_else(|| val.get("callID").or_else(|| val.get("id")).or_else(|| val.get("call_id")))
                .and_then(|i| i.as_str())
                .unwrap_or("call");

            let state_obj = part_obj.and_then(|p| p.get("state"));
            let input_val = state_obj
                .and_then(|s| s.get("input").or_else(|| s.get("args")).or_else(|| s.get("parameters")))
                .or_else(|| part_obj.and_then(|p| p.get("input").or_else(|| p.get("args")).or_else(|| p.get("parameters"))))
                .or_else(|| val.get("input").or_else(|| val.get("args")).or_else(|| val.get("parameters")));

            // Unwrap MCP tool name if this is call_mcp_tool or if ToolName is specified in params
            let mcp_tool_name = input_val
                .and_then(|p| p.get("ToolName").or_else(|| p.get("tool_name")).or_else(|| p.get("tool")))
                .and_then(|t| t.as_str());

            let display_tool_name = if let Some(mcp) = mcp_tool_name {
                mcp
            } else {
                tool_name
            };

            let actual_command = if let Some(args) = input_val {
                let args_obj = args.get("Arguments").or_else(|| args.get("arguments")).unwrap_or(args);
                args_obj.get("CommandLine")
                    .or_else(|| args_obj.get("command_line"))
                    .or_else(|| args_obj.get("command"))
                    .or_else(|| args_obj.get("cmd"))
                    .or_else(|| args_obj.get("script"))
                    .and_then(|c| c.as_str())
            } else {
                None
            };

            let entry_tool_name = if (display_tool_name == "run_command"
                || display_tool_name == "run_shell_command"
                || display_tool_name == "bash"
                || display_tool_name == "exec"
                || display_tool_name == "run")
                && actual_command.map_or(false, |c| !c.trim().is_empty())
            {
                actual_command.unwrap().to_string()
            } else {
                display_tool_name.to_string()
            };

            let lower = display_tool_name.to_lowercase();
            let category = if lower.contains("query_peer") || lower.contains("peer_symbol") || lower.contains("peer_file") {
                "peer"
            } else if lower == "task" || lower.contains("subagent") || lower.contains("delegate") {
                "task"
            } else if lower.contains("read") || lower.contains("view") {
                "read"
            } else if lower.contains("write") || lower.contains("edit") || lower.contains("replace") || lower.contains("lease") {
                "write"
            } else if lower.contains("test") {
                "test"
            } else if lower.contains("build") || lower.contains("compile") {
                "build"
            } else if lower.contains("git") {
                "git"
            } else if lower.contains("search") || lower.contains("grep") || lower.contains("find") || lower.contains("glob") {
                "search"
            } else if lower.contains("run") || lower.contains("bash") || lower.contains("exec") || lower.contains("command") {
                "run"
            } else if lower.contains("ask") || lower.contains("question") {
                "question"
            } else {
                "run"
            };

            let raw_cmd = if let Some(args) = input_val.and_then(|p| p.get("Arguments").or_else(|| p.get("arguments"))) {
                args.to_string()
            } else {
                input_val.map(|a| a.to_string()).unwrap_or_default()
            };

            let mut file_paths = Vec::new();
            if let Some(args) = input_val {
                let args_obj = args.get("Arguments").or_else(|| args.get("arguments")).unwrap_or(args);
                if let Some(path) = args_obj.get("filePath")
                    .or_else(|| args_obj.get("path"))
                    .or_else(|| args_obj.get("file_path"))
                    .or_else(|| args_obj.get("file"))
                    .or_else(|| args_obj.get("target_file"))
                    .or_else(|| args_obj.get("TargetFile"))
                    .and_then(|p| p.as_str())
                {
                    file_paths.push(path.to_string());
                }
                if let Some(paths) = args_obj.get("paths").and_then(|p| p.as_array()) {
                    for path in paths.iter().filter_map(|p| p.as_str()) {
                        file_paths.push(path.to_string());
                    }
                }
            }

            let status_str = state_obj
                .and_then(|s| s.get("status").or_else(|| s.get("state")))
                .or_else(|| part_obj.and_then(|p| p.get("status").or_else(|| p.get("state"))))
                .or_else(|| val.get("status").or_else(|| val.get("state")))
                .and_then(|s| s.as_str())
                .unwrap_or("");

            let output_str = state_obj
                .and_then(|s| s.get("output").or_else(|| s.get("result")))
                .or_else(|| part_obj.and_then(|p| p.get("output").or_else(|| p.get("result"))))
                .or_else(|| val.get("output").or_else(|| val.get("result")))
                .and_then(|o| o.as_str());

            let is_completed = status_str == "completed"
                || status_str == "done"
                || status_str == "success"
                || status_str == "finished"
                || output_str.is_some();

            if !is_completed {
                let is_new = {
                    let mut open = OPEN_TOOLS.lock().unwrap_or_else(|e| e.into_inner());
                    if !open.contains(&call_id.to_string()) {
                        open.push(call_id.to_string());
                        true
                    } else {
                        false
                    }
                };

                if is_new {
                    events.push(AgentEvent::NewEntry {
                        vendor_id: call_id.to_string(),
                        tool: entry_tool_name,
                        category: category.to_string(),
                        raw_cmd,
                        file_paths,
                    });
                }
            } else {
                let was_open = {
                    let mut open = OPEN_TOOLS.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(pos) = open.iter().position(|x| x == call_id) {
                        open.swap_remove(pos);
                        true
                    } else {
                        false
                    }
                };

                if !was_open {
                    events.push(AgentEvent::NewEntry {
                        vendor_id: call_id.to_string(),
                        tool: entry_tool_name,
                        category: category.to_string(),
                        raw_cmd,
                        file_paths,
                    });
                }

                let exit_code = val.get("exit_code")
                    .or_else(|| part_obj.and_then(|p| p.get("exit_code")))
                    .or_else(|| state_obj.and_then(|s| s.get("exit_code")))
                    .and_then(|c| c.as_i64())
                    .unwrap_or(0) as i32;

                let lines: Vec<String> = output_str
                    .map(|o| o.lines().map(strip_ansi).collect())
                    .unwrap_or_default();

                events.push(AgentEvent::CloseEntry {
                    vendor_id: call_id.to_string(),
                    exit_code,
                    output_lines: lines,
                });
            }

            return (STATE_NONE, events);
        } else if event_type == "tool_result" || (event_type == "result" && part_obj.and_then(|p| p.get("tool")).is_some()) {
            let call_id = part_obj
                .and_then(|p| p.get("callID").or_else(|| p.get("id")).or_else(|| p.get("call_id")))
                .or_else(|| val.get("id").or_else(|| val.get("call_id")))
                .and_then(|i| i.as_str())
                .unwrap_or("call");
            let output = part_obj
                .and_then(|p| p.get("output").or_else(|| p.get("result")))
                .or_else(|| val.get("output").or_else(|| val.get("result")))
                .and_then(|o| o.as_str())
                .unwrap_or("");
            let exit_code = val.get("exit_code").and_then(|c| c.as_i64()).unwrap_or(0) as i32;
            let lines: Vec<String> = output.lines().map(strip_ansi).collect();

            {
                let mut open = OPEN_TOOLS.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(pos) = open.iter().position(|x| x == call_id) {
                    open.swap_remove(pos);
                }
            }

            events.push(AgentEvent::CloseEntry {
                vendor_id: call_id.to_string(),
                exit_code,
                output_lines: lines,
            });
            return (STATE_NONE, events);
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
            return (STATE_NONE, events);
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
                if open_entry == STATE_THOUGHT_OPEN {
                    events.push(AgentEvent::AppendToEntry {
                        vendor_id: "agent-thought".to_string(),
                        text: cleaned,
                    });
                } else {
                    events.push(AgentEvent::NewEntry {
                        vendor_id: "agent-thought".to_string(),
                        tool: cleaned.chars().take(80).collect(),
                        category: "thought".into(),
                        raw_cmd: cleaned,
                        file_paths: Vec::new(),
                    });
                }
                return (STATE_THOUGHT_OPEN, events);
            }
            return (open_entry, events);
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
                if open_entry == STATE_MSG_OPEN {
                    events.push(AgentEvent::AppendToEntry {
                        vendor_id: "agent-response".to_string(),
                        text: cleaned,
                    });
                } else {
                    events.push(AgentEvent::NewEntry {
                        vendor_id: "agent-response".to_string(),
                        tool: cleaned.chars().take(80).collect(),
                        category: "message".into(),
                        raw_cmd: cleaned,
                        file_paths: Vec::new(),
                    });
                }
                return (STATE_MSG_OPEN, events);
            }
            return (open_entry, events);
        } else if event_type == "done" || event_type == "complete" || event_type == "finish"
            || event_type == "step_finish" || event_type == "step-finish"
            || part_obj.and_then(|p| p.get("type")).and_then(|t| t.as_str()) == Some("step-finish")
        {
            let reason = part_obj
                .and_then(|p| p.get("reason"))
                .or_else(|| val.get("reason"))
                .and_then(|r| r.as_str())
                .unwrap_or("stop");
            // A step boundary is NOT a turn boundary: `opencode run` streams
            // many steps per run (one per tool-call batch) and only exits the
            // process when the run is over. Emitting SessionEnded here ended
            // the turn (and SIGKILLed the child) at the first step. Terminal
            // state comes from the process exit (poll_exits) or the error
            // event below — so successful steps emit nothing.
            let is_terminal = reason == "error" || reason == "cancelled" || reason == "failed";
            if !is_terminal {
                return (STATE_NONE, events);
            }
            let error = {
                let err_msg = extract_opencode_error(&val, part_obj);
                if err_msg != "Unknown error" {
                    Some(err_msg)
                } else {
                    None
                }
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
            events.push(AgentEvent::SessionEnded { success: false, error });
            return (STATE_NONE, events);
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
            events.push(AgentEvent::SessionEnded { success: false, error: Some(cleaned) });
            return (STATE_NONE, events);
        } else {
            // Unrecognized JSON object - check for explicit message text; otherwise ignore metadata objects to avoid polluting chat log with raw JSON strings.
            if let Some(text) = part_obj.and_then(|p| p.get("text")).or_else(|| val.get("text")).or_else(|| val.get("message")).and_then(|t| t.as_str()) {
                let cleaned = strip_ansi(text);
                if !cleaned.trim().is_empty() {
                    if open_entry == STATE_MSG_OPEN {
                        events.push(AgentEvent::AppendToEntry {
                            vendor_id: "agent-response".to_string(),
                            text: cleaned,
                        });
                        return (STATE_MSG_OPEN, events);
                    }
                    events.push(AgentEvent::NewEntry {
                        vendor_id: format!("msg-{}", cleaned.len()),
                        tool: cleaned.chars().take(80).collect(),
                        category: "message".into(),
                        raw_cmd: cleaned,
                        file_paths: Vec::new(),
                    });
                    return (STATE_MSG_OPEN, events);
                }
            }
            return (STATE_NONE, events);
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

    (STATE_NONE, events)
}

pub fn parse_opencode_json_line(line_str: &str) -> Vec<AgentEvent> {
    parse_opencode_line_stateful(line_str, STATE_NONE).1
}

#[no_mangle]
pub extern "C" fn basalt_agent_parse_line(
    line_ptr: *const u8,
    line_len: u32,
    state_ptr: *const u8,
    state_len: u32,
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

    let open_entry = if !state_ptr.is_null() && state_len > 0 {
        let state_slice = unsafe { std::slice::from_raw_parts(state_ptr, state_len as usize) };
        state_slice[0]
    } else {
        STATE_NONE
    };

    let (new_state, events) = parse_opencode_line_stateful(line_str, open_entry);
    pack_output(encode_agent_parse_output(&[new_state], &events))
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

        // 4. step_finish with a success reason is a mid-run step boundary,
        // not a turn boundary: it must emit nothing (turn end comes from
        // process exit or the error event). Only terminal reasons end it.
        let step_finish_json = r#"{"type":"step_finish","timestamp":1789240423264,"sessionID":"ses_f68f5f178ffeDT2AycolEs49NT","part":{"id":"prt_0970a3353001THyTdtCclTi8h0","reason":"stop","messageID":"msg_0970a1bc6001hi1gQHwzKhRJx8","sessionID":"ses_f68f5f178ffeDT2AycolEs49NT","type":"step-finish","tokens":{"total":7256,"input":7229,"output":3,"reasoning":24,"cache":{"write":0,"read":0}},"cost":0}}"#;
        let evs = parse_opencode_json_line(step_finish_json);
        assert!(evs.is_empty(), "mid-run step must not end the turn");

        let step_fail_json = r#"{"type":"step_finish","timestamp":1789240423264,"sessionID":"ses_f68f5f178ffeDT2AycolEs49NT","part":{"id":"prt_0970a3353001THyTdtCclTi8h0","reason":"failed","messageID":"msg_0970a1bc6001hi1gQHwzKhRJx8","sessionID":"ses_f68f5f178ffeDT2AycolEs49NT","type":"step-finish","tokens":{"total":7256,"input":7229,"output":3,"reasoning":24,"cache":{"write":0,"read":0}},"cost":0}}"#;
        let evs = parse_opencode_json_line(step_fail_json);
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            AgentEvent::SessionEnded { success, .. } => assert!(!success),
            _ => panic!("expected failed SessionEnded"),
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
            workspace_path: None,
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
            workspace_path: None,
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

        let test_json = r#"{"type":"tool_use","part":{"type":"tool","tool":"run_cargo_test","callID":"c4","state":{"status":"completed","input":{"args":"--all"}}}}"#;
        let evs = parse_opencode_json_line(test_json);
        match &evs[0] {
            AgentEvent::NewEntry { tool, category, .. } => {
                assert_eq!(tool, "run_cargo_test");
                assert_eq!(category, "test");
            }
            _ => panic!("expected NewEntry for run_cargo_test"),
        }

        let git_json = r#"{"type":"tool_use","part":{"type":"tool","tool":"git_diff","callID":"c5","state":{"status":"completed","input":{}}}}"#;
        let evs = parse_opencode_json_line(git_json);
        match &evs[0] {
            AgentEvent::NewEntry { tool, category, .. } => {
                assert_eq!(tool, "git_diff");
                assert_eq!(category, "git");
            }
            _ => panic!("expected NewEntry for git_diff"),
        }

        let peer_json = r#"{"type":"tool_use","part":{"type":"tool","tool":"basalt_query_peer_symbol","callID":"c6","state":{"status":"completed","input":{"peer_id":7,"symbol":"auth_v2"}}}}"#;
        let evs = parse_opencode_json_line(peer_json);
        match &evs[0] {
            AgentEvent::NewEntry { tool, category, .. } => {
                assert_eq!(tool, "basalt_query_peer_symbol");
                assert_eq!(category, "peer");
            }
            _ => panic!("expected NewEntry for basalt_query_peer_symbol"),
        }

        let task_json = r#"{"type":"tool_use","part":{"type":"tool","tool":"task","callID":"c7","state":{"status":"completed","input":{"description":"explore auth module"}}}}"#;
        let evs = parse_opencode_json_line(task_json);
        match &evs[0] {
            AgentEvent::NewEntry { tool, category, .. } => {
                assert_eq!(tool, "task");
                assert_eq!(category, "task");
            }
            _ => panic!("expected NewEntry for task"),
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
            AgentEvent::SessionEnded { success, .. } => {
                assert!(!success);
            }
            _ => panic!("expected SessionEnded with failure"),
        }
    }

    #[test]
    fn test_parse_container_header_filtering() {
        // Container header object with sessionID but no part / tool payload should emit SessionIDAvailable, not raw chat line.
        let header_json = r#"{"type":"tool_use","timestamp":1789969295278,"sessionID":"ses_f3d8449e7ffegh"}"#;
        let evs = parse_opencode_json_line(header_json);
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            AgentEvent::SessionIDAvailable(sid) => assert_eq!(sid, "ses_f3d8449e7ffegh"),
            _ => panic!("expected SessionIDAvailable for container header"),
        }
    }

    #[test]
    fn test_parse_mcp_and_command_unwrapping() {
        // MCP tool unwrapping
        let mcp_json = r#"{"type":"tool_use","part":{"type":"tool","tool":"call_mcp_tool","callID":"c_mcp1","status":"completed","input":{"ToolName":"write_file","Arguments":{"path":"src/lib.rs"}},"output":"Written"}}"#;
        let evs = parse_opencode_json_line(mcp_json);
        assert_eq!(evs.len(), 2);
        match &evs[0] {
            AgentEvent::NewEntry { tool, category, file_paths, .. } => {
                assert_eq!(tool, "write_file");
                assert_eq!(category, "write");
                assert_eq!(file_paths, &vec!["src/lib.rs".to_string()]);
            }
            _ => panic!("expected NewEntry for MCP tool"),
        }

        // Command line unwrapping
        let cmd_json = r#"{"type":"tool_use","part":{"type":"tool","tool":"run_command","callID":"c_cmd1","status":"completed","input":{"CommandLine":"cargo check --workspace"},"output":"ok"}}"#;
        let evs = parse_opencode_json_line(cmd_json);
        assert_eq!(evs.len(), 2);
        match &evs[0] {
            AgentEvent::NewEntry { tool, category, .. } => {
                assert_eq!(tool, "cargo check --workspace");
                assert_eq!(category, "run");
            }
            _ => panic!("expected NewEntry for command tool"),
        }
    }

    #[test]
    fn test_stateful_streaming_deltas() {
        let (st1, evs1) = parse_opencode_line_stateful(
            r#"{"type":"text","part":{"type":"text","text":"Hello "}}"#,
            STATE_NONE,
        );
        assert_eq!(st1, STATE_MSG_OPEN);
        assert_eq!(evs1.len(), 1);
        match &evs1[0] {
            AgentEvent::NewEntry { category, raw_cmd, .. } => {
                assert_eq!(category, "message");
                assert_eq!(raw_cmd, "Hello ");
            }
            _ => panic!("expected NewEntry for first message delta"),
        }

        let (st2, evs2) = parse_opencode_line_stateful(
            r#"{"type":"text","part":{"type":"text","text":"world!"}}"#,
            st1,
        );
        assert_eq!(st2, STATE_MSG_OPEN);
        assert_eq!(evs2.len(), 1);
        match &evs2[0] {
            AgentEvent::AppendToEntry { vendor_id, text } => {
                assert_eq!(vendor_id, "agent-response");
                assert_eq!(text, "world!");
            }
            _ => panic!("expected AppendToEntry for second message delta"),
        }
    }

    #[test]
    fn test_two_phase_tool_lifecycle() {
        if let Ok(mut set) = OPEN_TOOLS.lock() {
            set.clear();
        }

        // Phase 1: running
        let active_json = r#"{"type":"tool_use","part":{"type":"tool","tool":"read","callID":"c_life1","status":"running","input":{"filePath":"src/main.rs"}}}"#;
        let (st1, evs1) = parse_opencode_line_stateful(active_json, STATE_NONE);
        assert_eq!(st1, STATE_NONE);
        assert_eq!(evs1.len(), 1);
        match &evs1[0] {
            AgentEvent::NewEntry { vendor_id, tool, category, file_paths, .. } => {
                assert_eq!(vendor_id, "c_life1");
                assert_eq!(tool, "read");
                assert_eq!(category, "read");
                assert_eq!(file_paths, &vec!["src/main.rs".to_string()]);
            }
            _ => panic!("expected NewEntry for tool start"),
        }

        // Phase 2: completed
        let completed_json = r#"{"type":"tool_use","part":{"type":"tool","tool":"read","callID":"c_life1","status":"completed","input":{"filePath":"src/main.rs"},"output":"fn main() {}"}}"#;
        let (st2, evs2) = parse_opencode_line_stateful(completed_json, st1);
        assert_eq!(st2, STATE_NONE);
        assert_eq!(evs2.len(), 1);
        match &evs2[0] {
            AgentEvent::CloseEntry { vendor_id, exit_code, output_lines } => {
                assert_eq!(vendor_id, "c_life1");
                assert_eq!(*exit_code, 0);
                assert_eq!(output_lines, &vec!["fn main() {}".to_string()]);
            }
            _ => panic!("expected CloseEntry for tool completion"),
        }
    }
}

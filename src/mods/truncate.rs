//! gray-truncate — caps oversized tool results before the model sees them.
//!
//! Claims `tool/after` (protocol 2.0): when a tool result's `content` exceeds
//! the configured cap it is replaced with head + marker + tail (70/30 split,
//! UTF-8 boundary safe). Port of pi's `truncated-tool` extension.
//!
//! `/truncate` → status · `/truncate set <n>` · `/truncate off|on`
//! Config: ~/.gray/truncate/max_chars (default 12000), ~/.gray/truncate/disabled

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

const DEFAULT_MAX: usize = 12_000;

fn manifest_truncate() -> Value {
    json!({
        "name": "truncate",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "2.0",
        "tools": [],
        "commands": ["/truncate"],
        "hooks": ["tool/after"],
    })
}

fn state_dir() -> PathBuf {
    let home = std::env::var_os("GRAY_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".gray")))
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("truncate")
}

fn enabled() -> bool {
    !state_dir().join("disabled").exists()
}

fn max_chars() -> usize {
    std::fs::read_to_string(state_dir().join("max_chars"))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(DEFAULT_MAX)
}

/// Split `s` into (head, tail) around `max` total kept chars, 70/30, on
/// UTF-8 boundaries. `max` is the total kept budget — the marker adds a line.
fn truncate_content(s: &str, max: usize) -> (String, usize) {
    let head_budget = max * 7 / 10;
    let tail_budget = max - head_budget;
    let head_end = floor_boundary(s, head_budget);
    let tail_start = ceil_boundary(s, s.len().saturating_sub(tail_budget));
    let tail_start = tail_start.max(head_end);
    let omitted = s.len() - head_end - (s.len() - tail_start);
    (
        format!(
            "{}\n\n[…truncated {omitted} chars, middle omitted…]\n\n{}",
            &s[..head_end],
            &s[tail_start..]
        ),
        omitted,
    )
}

fn floor_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// `tool/after` params {name, content, is_error, session}. Returns a
/// replacement result or None to keep the original.
fn tool_after(params: &Value) -> Option<Value> {
    if !enabled() {
        return None;
    }
    let content = params.get("content").and_then(Value::as_str)?;
    let max = max_chars();
    if content.chars().count() <= max {
        return None;
    }
    let (new_content, _omitted) = truncate_content(content, max);
    Some(json!({ "content": new_content }))
}

fn run_command(argv: &[&str]) -> String {
    match argv.first().copied() {
        Some("set") => {
            let n: usize = match argv.get(1).and_then(|s| s.parse().ok()) {
                Some(n) if n >= 100 => n,
                _ => return "usage: /truncate set <chars> (min 100)".into(),
            };
            match std::fs::create_dir_all(state_dir())
                .and_then(|_| std::fs::write(state_dir().join("max_chars"), n.to_string()))
            {
                Ok(()) => format!("max result size set to {n} chars"),
                Err(e) => format!("couldn't write config: {e}"),
            }
        }
        Some("off") | Some("on") => {
            let flag = state_dir().join("disabled");
            let res = if argv[0] == "off" {
                std::fs::create_dir_all(state_dir()).and_then(|_| std::fs::write(&flag, b""))
            } else {
                std::fs::remove_file(&flag).or_else(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }
                })
            };
            match res {
                Ok(()) => format!("truncation {}", if argv[0] == "off" { "off" } else { "on" }),
                Err(e) => format!("couldn't flip state: {e}"),
            }
        }
        _ => format!(
            "gray-truncate {} — tool results capped at {} chars, currently {}.\n\
             /truncate set <n> · /truncate off|on",
            env!("CARGO_PKG_VERSION"),
            max_chars(),
            if enabled() { "on" } else { "off" },
        ),
    }
}

/// One request → `Some(reply)`, or `None` for notifications. The bool asks
/// the loop to exit after writing the reply.
pub fn handle_truncate(req: &Value) -> (Option<Value>, bool) {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = id else {
        return (None, method == "plugin/shutdown");
    };
    let result = match method {
        "plugin/manifest" => manifest_truncate(),
        "tool/after" => match tool_after(&params) {
            Some(v) => v,
            None => json!({}),
        },
        "command/run" => {
            let argv: Vec<&str> = params
                .get("argv")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            json!({ "text": run_command(&argv) })
        }
        "plugin/shutdown" => return (Some(json!({ "id": id, "result": {} })), true),
        _ => {
            let error = json!({ "code": -32601, "message": "method not found" });
            return (Some(json!({ "id": id, "error": error })), false);
        }
    };
    (Some(json!({ "id": id, "result": result })), false)
}

fn main_truncate() -> std::io::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("manifest") {
        println!("{}", manifest_truncate());
        return Ok(());
    }
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let (reply, exit) = handle_truncate(&req);
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
        if exit {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(method: &str, params: Value) -> Value {
        handle_truncate(&json!({ "id": 1, "method": method, "params": params })).0.unwrap()
    }

    #[test]
    fn manifest_claims_tool_after() {
        let m = call("plugin/manifest", Value::Null)["result"].clone();
        assert_eq!(m["hooks"], json!(["tool/after"]));
        assert_eq!(m["protocol"], "2.0");
    }

    #[test]
    fn short_results_pass_through() {
        let r = call("tool/after", json!({ "name": "bash", "content": "small", "is_error": false }));
        assert_eq!(r["result"], json!({}));
    }

    #[test]
    fn long_results_are_capped_head_and_tail() {
        let head: String = std::iter::repeat_n('h', 9000).collect();
        let mid: String = std::iter::repeat_n('m', 20000).collect();
        let tail: String = std::iter::repeat_n('t', 5000).collect();
        let content = format!("{head}{mid}{tail}");
        let r = call("tool/after", json!({ "name": "read", "content": content, "is_error": false }));
        let out = r["result"]["content"].as_str().unwrap();
        assert!(out.contains("truncated"));
        assert!(out.starts_with(&head[..100]));
        assert!(out.ends_with(&tail[tail.len() - 100..]));
        assert!(out.chars().count() < 15_000);
    }

    #[test]
    fn multibyte_never_splits_a_char() {
        let mut s = String::new();
        for i in 0..6000 {
            s.push(if i % 3 == 0 { 'é' } else { 'x' });
        }
        let (out, _) = truncate_content(&s, 1200);
        let _ = out; // would have panicked already if a boundary was crossed
    }

    #[test]
    fn status_command_reports_state() {
        let r = call("command/run", json!({ "name": "/truncate", "argv": [] }));
        assert!(r["result"]["text"].as_str().unwrap().contains("chars"));
    }

    #[test]
    fn shutdown_replies_then_exits_and_notifications_are_silent() {
        let (reply, exit) = handle_truncate(&json!({ "id": 2, "method": "plugin/shutdown" }));
        assert!(reply.is_some() && exit);
        let (reply, exit) = handle_truncate(&json!({ "method": "plugin/shutdown" }));
        assert!(reply.is_none() && exit);
    }
}

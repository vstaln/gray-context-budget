//! gray-context-budget — one sidecar for keeping the context window in budget.
//!
//! Merges gray-truncate, gray-compact and gray-cacheopt. Each source lives
//! nearly verbatim in `mods/` with its own state files and slash commands —
//! only the wire plumbing is shared.
//!
//! Hook fan-out:
//! - `tool/after`      → truncate (cap oversized tool results).
//! - `context/build`   → chain: compact (may replace the oldest 70% with a
//!   summary) → cacheopt observes the list that actually goes out.
//! - `prompt/context`  → cacheopt (prefix-break warning).
//! - `event/notify`    → cacheopt (`turn_end` usage → cache stats).
//! - `command/run`     → routed by `params.name` to the owning module.
//!
//! compact calls back into the host (`host/run`), so the wire loop uses its
//! reader-thread shape: string-id replies route to pending waiters, every
//! other frame queues for the main loop.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex, mpsc};

use serde_json::{Value, json};

mod parts;

#[allow(dead_code)]
#[path = "mods/cacheopt.rs"]
mod cacheopt;
#[allow(dead_code)]
#[path = "mods/compact.rs"]
mod compact;
#[allow(dead_code)]
#[path = "mods/truncate.rs"]
mod truncate;

const META: &str = "/context-budget";

const PARTS: &[parts::Part] = &[
    parts::Part {
        name: "truncate",
        default: true,
        description: "cap oversized tool results before they enter context",
    },
    parts::Part {
        name: "compact",
        default: true,
        description: "summarize older context to shrink it",
    },
    parts::Part {
        name: "cacheopt",
        default: true,
        description: "prompt-cache hit rate and prefix-break warnings",
    },
];

fn manifest(parts: &parts::Parts) -> Value {
    let mut commands: Vec<&str> = vec![META];
    let mut descriptions = serde_json::Map::new();
    descriptions.insert(META.into(), json!("list the context-budget modules"));
    if parts.on("truncate") {
        commands.push("/truncate");
        descriptions.insert("/truncate".into(), json!("cap oversized tool results"));
    }
    if parts.on("compact") {
        commands.push("/compact");
        descriptions.insert(
            "/compact".into(),
            json!("summarize older context to shrink it"),
        );
    }
    if parts.on("cacheopt") {
        commands.push("/cachestats");
        descriptions.insert(
            "/cachestats".into(),
            json!("prompt-cache hit rate and prefix churn"),
        );
    }
    let mut completion: Vec<&str> = Vec::new();
    if parts.on("cacheopt") {
        completion.push("reset");
    }
    let mut hooks: Vec<&str> = Vec::new();
    if parts.on("truncate") {
        hooks.push("tool/after");
    }
    if parts.on("compact") || parts.on("cacheopt") {
        hooks.push("context/build");
    }
    if parts.on("cacheopt") {
        hooks.push("prompt/context");
    }
    let mut capabilities: Vec<&str> = Vec::new();
    if parts.on("compact") {
        capabilities.push("host.turn");
    }
    json!({
        "name": "context-budget",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "2.0",
        "sidecar_args": ["sidecar"],
        "tools": [],
        "description": "cap context size and track cache reuse",
        "commands": commands,
        "command_descriptions": descriptions,
        "completion": completion,
        "hooks": hooks,
        "capabilities": capabilities,
        "parts": parts.manifest(PARTS),
    })
}

/// `/context-budget` — merged-plugin overview.
fn meta_command(parts: &parts::Parts) -> String {
    let mut lines: Vec<String> = vec![format!(
        "gray-context-budget {} — one sidecar, three context-budget modules:",
        env!("CARGO_PKG_VERSION")
    )];
    if parts.on("truncate") {
        lines.push(
            "\x20 /truncate    cap oversized tool results (head/tail, ~/.gray/truncate/)".into(),
        );
    }
    if parts.on("compact") {
        lines.push(
            "\x20 /compact     auto-summarize oversized context via host/run (~/.gray/compact/)"
                .into(),
        );
    }
    if parts.on("cacheopt") {
        lines.push(
            "\x20 /cachestats  prompt-cache prefix-break warnings + hit-rate (~/.gray/cacheopt/)"
                .into(),
        );
    }
    lines.join("\n")
}

/// Unwrap a module reply to its `result` (or `{}`), for re-wrapping.
fn result_of(reply: Option<Value>) -> Value {
    reply
        .and_then(|r| r.get("result").cloned())
        .unwrap_or_else(|| json!({}))
}

/// `context/build` chain: compact → cacheopt. cacheopt is observe-only, so
/// the reply is compact's (`{messages}` or `{}`); cacheopt fingerprints the
/// list that will actually be sent.
fn context_build(
    req: &Value,
    watch: &cacheopt::Watch,
    link: Option<&compact::HostLink>,
    parts: &parts::Parts,
) -> Value {
    let result = if parts.on("compact") {
        let (reply, _) = compact::handle_compact(req, link);
        result_of(reply)
    } else {
        json!({})
    };
    if parts.on("cacheopt") {
        let mut next = req.clone();
        if let Some(msgs) = result.get("messages") {
            next["params"]["messages"] = msgs.clone();
        }
        let _ = cacheopt::handle_cacheopt(watch, &next);
    }
    result
}

fn handle(
    req: &Value,
    watch: &cacheopt::Watch,
    link: Option<&compact::HostLink>,
    parts: &parts::Parts,
) -> (Option<Value>, bool) {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = id else {
        // Notifications: event/notify turn_end feeds cacheopt's stats.
        if method == "event/notify" && parts.on("cacheopt") {
            let _ = cacheopt::handle_cacheopt(watch, req);
        }
        return (None, method == "plugin/shutdown");
    };
    let result = match method {
        "plugin/manifest" => manifest(parts),
        "tool/after" => {
            if !parts.on("truncate") {
                return (Some(json!({ "id": id, "result": {} })), false);
            }
            let (reply, _) = truncate::handle_truncate(req);
            return (reply, false);
        }
        "prompt/context" => {
            if !parts.on("cacheopt") {
                return (Some(json!({ "id": id, "result": {} })), false);
            }
            let (reply, _) = cacheopt::handle_cacheopt(watch, req);
            return (reply, false);
        }
        "context/build" => {
            if !parts.on("compact") && !parts.on("cacheopt") {
                return (Some(json!({ "id": id, "result": {} })), false);
            }
            context_build(req, watch, link, parts)
        }
        "command/run" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            match name {
                META => json!({ "text": meta_command(parts) }),
                "/truncate" if parts.on("truncate") => result_of(truncate::handle_truncate(req).0),
                "/compact" if parts.on("compact") => {
                    result_of(compact::handle_compact(req, link).0)
                }
                "/cachestats" if parts.on("cacheopt") => {
                    result_of(cacheopt::handle_cacheopt(watch, req).0)
                }
                _ => json!({ "text": format!("unknown command {name} — try {META}") }),
            }
        }
        "tool/call" => {
            // No module declares tools.
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let error = json!({ "code": -32601, "message": format!("unknown tool {name}") });
            return (Some(json!({ "id": id, "error": error })), false);
        }
        "plugin/shutdown" => return (Some(json!({ "id": id, "result": {} })), true),
        _ => {
            let error = json!({ "code": -32601, "message": "method not found" });
            return (Some(json!({ "id": id, "error": error })), false);
        }
    };
    (Some(json!({ "id": id, "result": result })), false)
}

fn main() {
    let parts = parts::Parts::load("context-budget", PARTS);
    if std::env::args().nth(1).as_deref() == Some("manifest") {
        println!("{}", manifest(&parts));
        return;
    }
    let stdout = Arc::new(Mutex::new(std::io::stdout()));
    let pending: compact::Pending = Arc::new(Mutex::new(HashMap::new()));
    let counter = Arc::new(Mutex::new(0u64));
    let watch = cacheopt::Watch::default();

    // Reader thread: `host/run` replies (string id, no method) route to
    // pending waiters; everything else queues for the main loop.
    let (work_tx, work_rx) = mpsc::channel::<Value>();
    let reader_pending = pending.clone();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(id) = v.get("id").and_then(|i| i.as_str())
                && v.get("method").is_none()
                && let Some(tx) = reader_pending.lock().expect("pending").remove(id)
            {
                let _ = tx.send(v);
                continue;
            }
            if work_tx.send(v).is_err() {
                break;
            }
        }
    });

    for req in work_rx {
        let link = compact::HostLink {
            out: stdout.clone(),
            pending: pending.clone(),
            counter: counter.clone(),
        };
        let (reply, exit) = handle(&req, &watch, Some(&link), &parts);
        if let Some(reply) = reply {
            let mut o = stdout.lock().expect("stdout");
            let _ = writeln!(o, "{reply}");
            let _ = o.flush();
        }
        if exit {
            break;
        }
    }
}

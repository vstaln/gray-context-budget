//! gray-cacheopt — prompt-cache watcher (port of pi-cache-optimizer).
//!
//! Watches `context/build` for the assembled outbound message list and
//! fingerprints the stable head of the prompt. When the shared prefix
//! shrinks between requests — a mid-history insertion, edit, or reorder —
//! the provider's prompt cache is invalidated, so the plugin raises a
//! warning through `prompt/context` on the next turn. `turn_end` usage is
//! accumulated into `~/.gray/cacheopt/state.json` for `/cachestats`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Mutex;

pub const PLUGIN_NAME: &str = "cacheopt";
pub const PROTOCOL: &str = "2.0"; // claims the mutating context/build hook
const RECENT_KEEP: usize = 24;

/// `~/.gray` honoring `$GRAY_HOME`.
pub fn gray_home() -> PathBuf {
    if let Some(p) = std::env::var_os("GRAY_HOME") {
        return PathBuf::from(p);
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".gray")
}

pub fn state_path() -> PathBuf {
    gray_home().join("cacheopt").join("state.json")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnRec {
    pub ts: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Stats {
    pub turns: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    #[serde(default)]
    pub recent: Vec<TurnRec>,
}

pub fn load_stats() -> Stats {
    std::fs::read_to_string(state_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_stats(s: &Stats) {
    let p = state_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, serde_json::to_string(s).unwrap_or_default());
}

/// Fold one `turn_end` usage object into the stats.
pub fn record_turn(s: &mut Stats, u: &Value) {
    let g = |k: &str| u.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
    let rec = TurnRec {
        ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        input: g("input_tokens"),
        output: g("output_tokens"),
        cache_read: g("cache_read_input_tokens"),
        cache_write: g("cache_write_input_tokens"),
    };
    s.turns += 1;
    s.input += rec.input;
    s.output += rec.output;
    s.cache_read += rec.cache_read;
    s.cache_write += rec.cache_write;
    s.recent.push(rec);
    if s.recent.len() > RECENT_KEEP {
        s.recent.drain(..s.recent.len() - RECENT_KEEP);
    }
}

/// FNV-1a over a message's role + text parts — stable in-process; only ever
/// compared against fingerprints computed by this same sidecar instance.
fn msg_hash(m: &Value) -> u64 {
    let mut h = DefaultHasher::new();
    m.get("role").and_then(|r| r.as_str()).unwrap_or("").hash(&mut h);
    if let Some(parts) = m.get("content").and_then(|c| c.as_array()) {
        for p in parts {
            if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                t.hash(&mut h);
            } else if let Some(t) = p.get("type").and_then(|t| t.as_str()) {
                t.hash(&mut h);
            }
        }
    } else if let Some(t) = m.get("content").and_then(|c| c.as_str()) {
        t.hash(&mut h);
    }
    h.finish()
}

pub fn fingerprint(messages: &Value) -> Vec<u64> {
    messages
        .as_array()
        .map(|ms| ms.iter().map(msg_hash).collect())
        .unwrap_or_default()
}

fn shared_prefix(a: &[u64], b: &[u64]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

/// In-memory per-session prefix tracking. Lost on sidecar restart: the next
/// context/build re-baselines silently (no false warning after a respawn).
#[derive(Default)]
pub struct Watch {
    last: Mutex<HashMap<String, Vec<u64>>>,
    pending: Mutex<HashMap<String, String>>,
}

impl Watch {
    /// Observe one assembled outbound message list. Records a warning when
    /// the shared head shrank vs. the previous request — i.e. the change
    /// was not a pure append, so the cached prefix is invalidated.
    pub fn observe(&self, session: &str, messages: &Value) {
        let cur = fingerprint(messages);
        if cur.is_empty() {
            return;
        }
        let mut last = self.last.lock().expect("watch");
        if let Some(prev) = last.get(session) {
            let shared = shared_prefix(prev, &cur);
            let shorter = prev.len().min(cur.len());
            if shared < shorter && !prev.is_empty() {
                let w = format!(
                    "cacheopt: prompt prefix changed — only {shared}/{} leading \
                     messages match the previous request (mid-history edit or \
                     reorder). The provider prompt cache is invalidated; keep \
                     system/context/history ordering stable to keep hit-rates up.",
                    prev.len().max(cur.len())
                );
                self.pending
                    .lock()
                    .expect("pending")
                    .insert(session.to_string(), w);
            }
        }
        last.insert(session.to_string(), cur);
    }

    /// Drain the pending warning for a session (prompt/context consumes it).
    pub fn take_warning(&self, session: &str) -> Option<String> {
        self.pending.lock().expect("pending").remove(session)
    }
}

pub fn manifest_cacheopt() -> Value {
    json!({
        "name": PLUGIN_NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": PROTOCOL,
        "sidecar_args": ["sidecar"],
        "tools": [],
        "commands": ["/cachestats"],
        "completion": ["reset"],
        "hooks": ["prompt/context", "context/build"],
    })
}

/// `/cachestats` table: totals, hit-rate, and the last few turns.
pub fn format_stats(s: &Stats) -> String {
    if s.turns == 0 {
        return "cacheopt: no turns recorded yet".to_string();
    }
    let hit = if s.input > 0 {
        s.cache_read as f64 / s.input as f64 * 100.0
    } else {
        0.0
    };
    let mut out = format!(
        "cacheopt — {} turns\n  hit-rate {:>5.1}%  ({} / {} input tokens served from cache)\n  totals  in={} out={} cache_read={} cache_write={}\n",
        s.turns, hit, s.cache_read, s.input, s.input, s.output, s.cache_read, s.cache_write
    );
    out.push_str("  recent   input   cached    hit%  output\n");
    for r in s.recent.iter().rev().take(12) {
        let h = if r.input > 0 {
            r.cache_read as f64 / r.input as f64 * 100.0
        } else {
            0.0
        };
        out.push_str(&format!(
            "        {:>7} {:>8} {:>6.0}% {:>7}\n",
            r.input, r.cache_read, h, r.output
        ));
    }
    out
}

// --- from src/main.rs ---
// `gray-cacheopt` sidecar: JSON-lines on stdio. No args (or `sidecar`)
// runs the wire loop; `stats`/`reset` are the standalone CLI.

// (merged into one module: `g::` now names this file itself)
use crate::cacheopt as g;
use std::io::{BufRead, Write};

fn session_id(req: &Value) -> &str {
    req["params"]["session"]["id"].as_str().unwrap_or("")
}

fn run_command(req: &Value) -> Result<Value, String> {
    let argv: Vec<&str> = req["params"]["argv"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    match argv.first().copied() {
        Some("reset") => {
            g::save_stats(&g::Stats::default());
            Ok(json!({"text": "cacheopt: stats cleared"}))
        }
        _ => Ok(json!({"text": g::format_stats(&g::load_stats())})),
    }
}

fn on_notify(req: &Value) {
    if req["method"] != "event/notify" || req["params"]["type"] != "turn_end" {
        return;
    }
    let mut s = g::load_stats();
    g::record_turn(&mut s, &req["params"]["usage"]);
    g::save_stats(&s);
}

/// One request → `Some(reply)`; notifications (`event/notify`) are folded
/// into stats and get `None`. Extracted verbatim from the sidecar loop below.
pub fn handle_cacheopt(watch: &Watch, req: &Value) -> (Option<Value>, bool) {
    if req["method"] == "plugin/shutdown" {
        return (None, true);
    }
    if req.get("id").is_none() {
        on_notify(req); // event/notify: fire-and-forget
        return (None, false);
    }
    let id = req["id"].clone();
    let result = match req["method"].as_str().unwrap_or("") {
        "plugin/manifest" => Ok(g::manifest_cacheopt()),
        "prompt/context" => Ok(json!({
            "text": watch.take_warning(session_id(req)).unwrap_or_default()
        })),
        "context/build" => {
            watch.observe(session_id(req), &req["params"]["messages"]);
            Ok(json!({})) // observe-only: keep the outbound list
        }
        "command/run" => run_command(req),
        _ => Err(format!("unsupported method {}", req["method"])),
    };
    let reply = match result {
        Ok(r) => json!({"id": id, "result": r}),
        Err(e) => json!({"id": id, "error": {"code": -32601, "message": e}}),
    };
    (Some(reply), false)
}

fn sidecar() -> std::io::Result<()> {
    let watch = g::Watch::default();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let (reply, exit) = handle_cacheopt(&watch, &req);
        if exit {
            break;
        }
        if let Some(reply) = reply {
            let mut o = stdout.lock();
            let _ = writeln!(o, "{reply}");
            let _ = o.flush();
        }
    }
    Ok(())
}

fn main_cacheopt() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    match arg.as_str() {
        "manifest" => println!("{}", g::manifest_cacheopt()),
        "stats" | "cachestats" => print!("{}", g::format_stats(&g::load_stats())),
        "reset" => {
            g::save_stats(&g::Stats::default());
            println!("cacheopt: stats cleared");
        }
        _ => {
            if let Err(e) = sidecar() {
                eprintln!("gray-cacheopt: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(texts: &[&str]) -> Value {
        json!(texts
            .iter()
            .map(|t| json!({"role": "user", "content": [{"type": "text", "text": t}]}))
            .collect::<Vec<_>>())
    }

    #[test]
    fn pure_append_is_healthy() {
        let w = Watch::default();
        w.observe("s", &msgs(&["a", "b"]));
        w.observe("s", &msgs(&["a", "b", "c"]));
        assert!(w.take_warning("s").is_none());
    }

    #[test]
    fn mid_history_edit_warns() {
        let w = Watch::default();
        w.observe("s", &msgs(&["a", "b", "c"]));
        w.observe("s", &msgs(&["a", "X", "c"]));
        assert!(w.take_warning("s").unwrap().contains("prefix changed"));
        assert!(w.take_warning("s").is_none()); // drained once
    }

    #[test]
    fn reorder_warns() {
        let w = Watch::default();
        w.observe("s", &msgs(&["a", "b", "c"]));
        w.observe("s", &msgs(&["b", "a", "c"]));
        assert!(w.take_warning("s").is_some());
    }

    #[test]
    fn first_observe_is_quiet() {
        let w = Watch::default();
        w.observe("s", &msgs(&["a"]));
        assert!(w.take_warning("s").is_none());
    }

    #[test]
    fn stats_accumulate_and_cap() {
        let mut s = Stats::default();
        for _ in 0..30 {
            record_turn(&mut s, &json!({"input_tokens": 100, "output_tokens": 10,
                "cache_read_input_tokens": 80, "cache_write_input_tokens": 5}));
        }
        assert_eq!(s.turns, 30);
        assert_eq!(s.cache_read, 2400);
        assert_eq!(s.recent.len(), RECENT_KEEP);
        assert!(format_stats(&s).contains("hit-rate  80.0%"));
    }

    #[test]
    fn manifest_claims_context_build() {
        let m = manifest_cacheopt();
        assert_eq!(m["protocol"], "2.0");
        assert!(m["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h == "context/build"));
    }
}

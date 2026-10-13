//! MCP (Model Context Protocol) front-end of the headless harness.
//!
//! Speaks JSON-RPC 2.0 over stdio, one message per line, mirroring
//! the `oryxis-mcp` plugin's server shape (initialize with version
//! negotiation, `ping`, `tools/list`, `tools/call`). The point: an
//! AI agent connects to `oryxis --harness-mcp` as an MCP server and
//! drives the real app headless through typed tools, either to
//! validate changes visually (the `screenshot` tool returns the PNG
//! inline as MCP image content) or to build up an interaction that
//! `save_ice` turns into a replayable `.ice` test.
//!
//! Note stdout discipline: MCP owns stdout entirely, which is why
//! `main.rs` routes the tracing subscriber to stderr whenever a
//! harness mode is active.

use std::io::BufRead as _;
use std::path::PathBuf;
use std::time::Duration;

use base64::Engine as _;
use iced::Program;
use iced_test::core::clipboard::Content;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Options, Pump, RunOutcome, Session, format_text_entry};

/// Protocol revisions this server implements. Tools-only over stdio
/// is wire-identical across all three; listing them lets version
/// negotiation echo whatever the client asked for instead of forcing
/// a downgrade to the oldest revision.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

#[derive(Debug, Deserialize)]
struct Request {
    id: Option<Value>,
    method: String,
    params: Option<Value>,
}

pub(super) fn serve<P>(program: P, options: Options) -> iced::Result
where
    P: Program + 'static,
{
    let (mut session, boot) = Session::new(&program, &options);
    let boot_note = match boot {
        Pump::Ready => "the app booted and is idle".to_owned(),
        Pump::Timeout => "the app booted but tasks were still settling".to_owned(),
        Pump::Failed(instruction) => format!("boot reported a failure: {instruction}"),
        Pump::Closed => {
            eprintln!("oryxis harness: emulator channel closed during boot");
            std::process::exit(1);
        }
    };

    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        // Absorb whatever the subscriptions produced while we were
        // blocked on stdin, so tools act on fresh state.
        session.drain(&program);

        line.clear();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let raw = line.trim();
        if raw.is_empty() {
            continue;
        }

        let request: Request = match serde_json::from_str(raw) {
            Ok(request) => request,
            Err(error) => {
                emit(&json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("parse error: {error}") }
                }));
                continue;
            }
        };

        // Notifications (no id) get no response, per JSON-RPC.
        let Some(id) = request.id else { continue };

        let response = match request.method.as_str() {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": negotiate_protocol_version(request.params.as_ref()),
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": "oryxis-harness",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "instructions": instructions_text(&options, &boot_note),
                }
            }),
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            "tools/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": { "tools": tool_definitions() }
            }),
            "tools/call" => {
                let name = request
                    .params
                    .as_ref()
                    .and_then(|p| p.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let arguments = request
                    .params
                    .as_ref()
                    .and_then(|p| p.get("arguments"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let result = call_tool(&mut session, &program, name, &arguments);
                json!({ "jsonrpc": "2.0", "id": id, "result": result })
            }
            other => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("method not found: {other}") }
            }),
        };

        emit(&response);
    }

    Ok(())
}

/// Version negotiation per the MCP spec: echo the client's requested
/// version when we support it, otherwise answer with the latest one
/// we do (the client then decides whether to proceed or disconnect).
fn negotiate_protocol_version(params: Option<&Value>) -> &'static str {
    let requested = params
        .and_then(|p| p.get("protocolVersion"))
        .and_then(Value::as_str);
    SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .find(|v| Some(**v) == requested)
        .copied()
        .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[SUPPORTED_PROTOCOL_VERSIONS.len() - 1])
}

fn instructions_text(options: &Options, boot_note: &str) -> String {
    format!(
        "Headless emulator of the real Oryxis app ({boot_note}). $HOME is \
         sandboxed at {home}, so nothing touches the real ~/.oryxis; state \
         persists across sessions until `reset` with wipe=true. Drive the UI \
         with `run` (ice instructions, one per line): click [right] \
         \"Text\"|#id|(x, y); click double <target> (both clicks on one \
         frame); move/press/release <target>; scroll [pixels] \
         (dx, dy) [target]; type \"text\"; type enter|escape|tab|backspace; \
         chords like `type ctrl+k`; expect \"Text\" asserts a visible text \
         widget (exact match; canvas content like the terminal grid is NOT \
         visible to it, use `screenshot`). After async work (vault unlock, \
         connections) call `settle`. Once a terminal session is open, call \
         set_timeout with ms=500: the live PTY keeps zen mode from ever \
         quiescing and each instruction would burn the full timeout. \
         `screenshot` returns the PNG inline. `save_ice` writes the recorded \
         instruction history as a replayable test for \
         `oryxis --harness-run`.",
        home = options.home.display(),
    )
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "run",
            "description": "Execute one or more .ice instructions against the emulated app, one per line (click \"Text\" / click #id / click (x, y) / click right <target> / click double <target> / move / press / release / scroll [pixels] (dx, dy) [target] / type \"text\" / type enter|escape|tab|backspace / type ctrl+shift+f / expect \"Text\"). Stops at the first failure. Instructions that execute are recorded for save_ice.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "script": { "type": "string", "description": ".ice instructions, one per line; blank lines and # comments are skipped" }
                },
                "required": ["script"]
            }
        },
        {
            "name": "screenshot",
            "description": "Render the current UI headless and return the PNG inline (also saved under the shots directory). The primary way to validate visually and to read terminal/canvas content.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "optional file stem for the saved PNG" }
                }
            }
        },
        {
            "name": "texts",
            "description": "Dump every visible text widget with its bounds in reading order, the DOM inspector for picking click targets and expect strings. Optionally filter to entries containing a substring.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filter": { "type": "string", "description": "only return entries containing this substring" }
                }
            }
        },
        {
            "name": "settle",
            "description": "Pump emulator events until the stream stays quiet (lets async work like a vault unlock, a connection or terminal output land). Prefer this over fixed waits.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "idle_ms": { "type": "number", "description": "quiet window that counts as settled (default 250, 10..5000)" }
                }
            }
        },
        {
            "name": "wait",
            "description": "Pump emulator events for a fixed duration in milliseconds.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ms": { "type": "number", "description": "milliseconds to pump" }
                },
                "required": ["ms"]
            }
        },
        {
            "name": "set_timeout",
            "description": "Set the per-instruction completion timeout in ms (default 20000). Set to 500 once a terminal session is open: its live PTY keeps zen mode from quiescing, so each instruction would otherwise burn the full timeout.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "ms": { "type": "number", "description": "timeout in milliseconds (100..600000)" }
                },
                "required": ["ms"]
            }
        },
        {
            "name": "clipboard_get",
            "description": "Read the emulated clipboard (what the app wrote via iced clipboard tasks or widget copies).",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "clipboard_set",
            "description": "Seed the emulated clipboard with text, so a following `type ctrl+v` pastes it into the focused widget.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "clipboard text content" }
                },
                "required": ["text"]
            }
        },
        {
            "name": "history",
            "description": "List the instructions recorded so far (what save_ice would write), optionally clearing them.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "clear": { "type": "boolean", "description": "clear the history after listing" }
                }
            }
        },
        {
            "name": "save_ice",
            "description": "Write the recorded instruction history as a .ice test file replayable with `oryxis --harness-run <dir>`. Harness pacing lines (settle / wait / timeout / screenshot) are recorded too, so terminal-session flows replay with the same rhythm.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "target file path, e.g. crates/oryxis-app/tests/e2e/flow.ice" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "reset",
            "description": "Reboot the emulated app in place and clear the recorded history. With wipe=true the sandbox .oryxis is removed first, so the app comes back in its first-run (onboarding) state, the way to start a reproducible flow.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "wipe": { "type": "boolean", "description": "wipe the sandbox vault first (default false)" }
                }
            }
        }
    ])
}

fn call_tool<P>(session: &mut Session<P>, program: &P, name: &str, arguments: &Value) -> Value
where
    P: Program + 'static,
{
    match name {
        "run" => {
            let Some(script) = arguments.get("script").and_then(Value::as_str) else {
                return error_result("run wants a `script` string");
            };
            let mut lines_run = 0usize;
            let mut report = String::new();
            for line in script.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let outcome = session.run_line(program, line);
                lines_run += 1;
                let status = match &outcome {
                    RunOutcome::Done => "ok",
                    RunOutcome::Timeout => "timeout (executed; tasks still pending)",
                    RunOutcome::Failed(_) => "FAILED",
                    RunOutcome::Closed => "emulator closed",
                    RunOutcome::Parse(e) => &format!("parse error: {e}"),
                };
                report.push_str(&format!("{status}: {line}\n"));
                match outcome {
                    RunOutcome::Done | RunOutcome::Timeout => {}
                    _ => return error_result(report.trim_end()),
                }
            }
            if lines_run == 0 {
                return error_result("run: no instructions in script");
            }
            text_result(report.trim_end())
        }
        "screenshot" => {
            let name = arguments
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match session.screenshot(program, name) {
                Ok((path, png)) => {
                    session.record(if name.is_empty() {
                        "screenshot".to_owned()
                    } else {
                        format!("screenshot {name}")
                    });
                    json!({
                        "content": [
                            {
                                "type": "image",
                                "data": base64::engine::general_purpose::STANDARD.encode(&png),
                                "mimeType": "image/png"
                            },
                            { "type": "text", "text": format!("saved to {}", path.display()) }
                        ]
                    })
                }
                Err(reason) => error_result(&reason),
            }
        }
        "texts" => {
            let filter = arguments.get("filter").and_then(Value::as_str);
            match session.texts(program) {
                Ok(entries) => {
                    let lines: Vec<String> = entries
                        .into_iter()
                        .filter(|(text, _)| filter.is_none_or(|f| text.contains(f)))
                        .map(|(text, bounds)| format_text_entry(&text, bounds))
                        .collect();
                    text_result(&format!(
                        "{}\n({} entries)",
                        lines.join("\n"),
                        lines.len()
                    ))
                }
                Err(reason) => error_result(&reason),
            }
        }
        "settle" => {
            let idle = arguments
                .get("idle_ms")
                .and_then(Value::as_u64)
                .unwrap_or(250)
                .clamp(10, 5_000);
            session.settle(
                program,
                Duration::from_millis(idle),
                Duration::from_secs(30),
            );
            session.record(format!("settle {idle}"));
            text_result("settled")
        }
        "wait" => {
            let Some(ms) = arguments.get("ms").and_then(Value::as_u64) else {
                return error_result("wait wants `ms`");
            };
            session.wait(program, Duration::from_millis(ms.min(600_000)));
            session.record(format!("wait {}", ms.min(600_000)));
            text_result("waited")
        }
        "set_timeout" => {
            let Some(ms) = arguments.get("ms").and_then(Value::as_u64) else {
                return error_result("set_timeout wants `ms`");
            };
            session.timeout = Duration::from_millis(ms.clamp(100, 600_000));
            session.record(format!("timeout {}", ms.clamp(100, 600_000)));
            text_result(&format!("instruction timeout set to {ms} ms"))
        }
        "clipboard_get" => match session.emulator.clipboard() {
            Some(Content::Text(text)) => text_result(text),
            Some(Content::Html(html)) => text_result(&format!("html: {html}")),
            Some(Content::Files(files)) => text_result(&format!("files: {files:?}")),
            Some(_) => text_result("<non-text content>"),
            None => text_result("<empty>"),
        },
        "clipboard_set" => {
            let Some(text) = arguments.get("text").and_then(Value::as_str) else {
                return error_result("clipboard_set wants `text`");
            };
            session
                .emulator
                .set_clipboard(Some(Content::Text(text.to_owned())));
            text_result("clipboard seeded")
        }
        "history" => {
            let listing = if session.history.is_empty() {
                "(no instructions recorded)".to_owned()
            } else {
                session
                    .history
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            if arguments
                .get("clear")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                session.history.clear();
            }
            text_result(&listing)
        }
        "save_ice" => {
            let Some(path) = arguments.get("path").and_then(Value::as_str) else {
                return error_result("save_ice wants `path`");
            };
            match session.save_ice(&PathBuf::from(path)) {
                Ok(content) => text_result(&format!("wrote {path}:\n{content}")),
                Err(reason) => error_result(&reason),
            }
        }
        "reset" => {
            let wipe = arguments
                .get("wipe")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            match session.reset(program, wipe) {
                Ok(Pump::Ready) => text_result(if wipe {
                    "rebooted on a wiped sandbox (first-run state)"
                } else {
                    "rebooted on the existing sandbox"
                }),
                Ok(Pump::Timeout) => text_result("rebooted; boot tasks still settling"),
                Ok(Pump::Failed(instruction)) => {
                    error_result(&format!("boot failure: {instruction}"))
                }
                Ok(Pump::Closed) => error_result("emulator channel closed"),
                Err(reason) => error_result(&reason),
            }
        }
        other => error_result(&format!("unknown tool: {other}")),
    }
}

fn text_result(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn error_result(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

/// One JSON-RPC message per stdout line, flushed immediately (stdout
/// is block-buffered when piped).
fn emit(value: &Value) {
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{value}");
    let _ = stdout.flush();
}

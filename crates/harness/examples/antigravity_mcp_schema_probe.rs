//! Synthetic MCP registration probe for Antigravity ACP. Print a fixture with
//! `schema plain 1399`, or run one live sample with
//! `live /absolute/path/to/agy_acp_server plain 1399 [server args...]`.
//! Start with a small plain control, then bisect byte sizes using plain.
//! Compare any-of, union-type and bare-items at the same known-good size.
//! A tools/list request shows discovery; only a tool call confirms usability.
#![allow(clippy::disallowed_methods)]

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const TOOL: &str = "schema_echo";

fn schema(case: &str, bytes: usize) -> Result<Value> {
    let field = match case {
        "plain" => json!({"type": "string"}),
        "any-of" => json!({"anyOf": [{"type": "string"}, {"type": "integer"}]}),
        "union-type" => json!({"type": ["string", "integer"]}),
        "bare-items" => json!({"type": "array", "items": {}}),
        "typed-items" => json!({"type": "array", "items": {"type": "string"}}),
        _ => bail!("unknown schema case: {case}"),
    };
    let mut value = json!({
        "type": "object",
        "description": "",
        "properties": {"value": field}
    });
    let min = value.to_string().len();
    ensure!(
        (min..=65_536).contains(&bytes),
        "size must be {min}–65536 bytes"
    );
    value["description"] = Value::String("x".repeat(bytes - min));
    Ok(value)
}

fn mcp(case: &str, bytes: usize) -> Result<()> {
    let schema = schema(case, bytes)?;
    let mut stdout = io::stdout().lock();
    for line in io::stdin().lock().lines() {
        let request: Value = serde_json::from_str(&line?)?;
        let Some(id) = request.get("id") else {
            continue;
        };
        let result = match request["method"].as_str() {
            Some("initialize") => json!({
                "protocolVersion": request["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "schema-probe", "version": "0"}
            }),
            Some("ping") => json!({}),
            Some("tools/list") => {
                std::fs::write("listed", "")?;
                json!({"tools": [{
                    "name": TOOL,
                    "description": "Return a fixed probe acknowledgement. No arguments needed.",
                    "inputSchema": schema,
                }]})
            }
            Some("tools/call") if request["params"]["name"] == TOOL => {
                std::fs::write("called", "")?;
                json!({"content": [{"type": "text", "text": "probe acknowledgement"}]})
            }
            _ => {
                writeln!(
                    stdout,
                    "{}",
                    json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": {"code": -32601, "message": "Unknown method or tool"}
                    })
                )?;
                stdout.flush()?;
                continue;
            }
        };
        writeln!(
            stdout,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        )?;
        stdout.flush()?;
    }
    Ok(())
}

async fn live(program: &Path, args: &[String], case: &str, bytes: usize) -> Result<()> {
    ensure!(
        program.is_absolute() && program.is_file(),
        "supply an absolute ACP executable path"
    );
    schema(case, bytes)?;
    let workspace = tempfile::tempdir()?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .current_dir(workspace.path())
        .env("BROWSER", "/usr/bin/true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for (name, _) in std::env::vars_os() {
        if name
            .to_str()
            .is_some_and(|name| name.starts_with("MAST_") || name.starts_with("ZERON_"))
        {
            command.env_remove(name);
        }
    }
    let mut child = command.spawn()?;
    let mut input = child.stdin.take().context("ACP stdin")?;
    let mut lines = BufReader::new(child.stdout.take().context("ACP stdout")?).lines();
    let mut id = 0;
    let mut call = async |method: &str, params: Value| -> Result<Value> {
        id += 1;
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        input.write_all(format!("{frame}\n").as_bytes()).await?;
        while let Some(line) = lines.next_line().await? {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(method) = message["method"].as_str() {
                if let Some(request_id) = message.get("id") {
                    let mut response = json!({"jsonrpc": "2.0", "id": request_id});
                    if method == "session/request_permission" {
                        response["result"] = json!({"outcome": {"outcome": "cancelled"}});
                    } else {
                        response["error"] =
                            json!({"code": -32601, "message": "Unsupported probe method"});
                    }
                    input.write_all(format!("{response}\n").as_bytes()).await?;
                }
            } else if message["id"] == id {
                ensure!(
                    message.get("result").is_some(),
                    "{method} failed (ACP code {})",
                    message["error"]["code"]
                );
                return Ok(message["result"].clone());
            }
        }
        bail!("ACP closed stdout during {method}")
    };
    let result = tokio::time::timeout(Duration::from_secs(120), async {
        call("initialize", json!({"protocolVersion": 1, "clientCapabilities": {}})).await?;
        let session = call("session/new", json!({
            "cwd": workspace.path(),
            "mcpServers": [{
                "name": "schema-probe",
                "command": std::env::current_exe()?,
                "args": ["stdio", case, &bytes.to_string(), workspace.path().to_str().context("probe directory is not UTF-8")?],
                "env": []
            }]
        })).await?;
        let session_id = session["sessionId"].as_str().context("missing ACP session ID")?;
        call("session/prompt", json!({
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": "Call the schema_echo tool with {} once. If unavailable, reply unavailable. Do not use other tools."}]
        })).await
    }).await;
    println!(
        "{case} {bytes} bytes: listed={} called={}",
        workspace.path().join("listed").exists(),
        workspace.path().join("called").exists(),
    );
    result.context("ACP probe timed out")??;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [mode, case, bytes] if mode == "schema" => println!("{}", schema(case, bytes.parse()?)?),
        [mode, case, bytes, workspace] if mode == "stdio" => {
            std::env::set_current_dir(workspace)?;
            mcp(case, bytes.parse()?)?;
        }
        [mode, program, case, bytes, rest @ ..] if mode == "live" => {
            live(Path::new(program), rest, case, bytes.parse()?).await?;
        }
        _ => bail!(
            "usage: schema <case> <bytes> | live <absolute ACP executable> <case> <bytes> [ACP args...]"
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn catalog_probe_fixtures_hold_byte_size_constant_across_keywords() {
        for case in ["plain", "any-of", "union-type", "bare-items", "typed-items"] {
            let schema = super::schema(case, 1399).unwrap();
            assert_eq!(schema.to_string().len(), 1399, "{case}");
        }
    }
}

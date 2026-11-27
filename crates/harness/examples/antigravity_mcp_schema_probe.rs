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
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

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

fn mcp(
    case: &str,
    bytes: usize,
    workspace: &Path,
    input: impl BufRead,
    mut output: impl Write,
) -> Result<()> {
    let schema = schema(case, bytes)?;
    for line in input.lines() {
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
                std::fs::write(workspace.join("listed"), "")?;
                json!({"tools": [{
                    "name": TOOL,
                    "description": "Return a fixed probe acknowledgement. No arguments needed.",
                    "inputSchema": schema,
                }]})
            }
            Some("tools/call") if request["params"]["name"] == TOOL => {
                std::fs::write(workspace.join("called"), "")?;
                json!({"content": [{"type": "text", "text": "probe acknowledgement"}]})
            }
            _ => {
                writeln!(
                    output,
                    "{}",
                    json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": {"code": -32601, "message": "Unknown method or tool"}
                    })
                )?;
                output.flush()?;
                continue;
            }
        };
        writeln!(
            output,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "result": result})
        )?;
        output.flush()?;
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
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        probe_session(
            child.stdin.take().context("ACP stdin")?,
            child.stdout.take().context("ACP stdout")?,
            workspace.path(),
            case,
            bytes,
        ),
    )
    .await;
    println!(
        "{case} {bytes} bytes: listed={} called={}",
        workspace.path().join("listed").exists(),
        workspace.path().join("called").exists(),
    );
    result.context("ACP probe timed out")??;
    Ok(())
}

async fn probe_session(
    mut input: impl AsyncWrite + Unpin,
    output: impl AsyncRead + Unpin,
    workspace: &Path,
    case: &str,
    bytes: usize,
) -> Result<()> {
    let mut lines = BufReader::new(output).lines();
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
    call(
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    )
    .await?;
    let session = call("session/new", json!({
        "cwd": workspace,
        "mcpServers": [{
            "name": "schema-probe",
            "command": std::env::current_exe()?,
            "args": ["stdio", case, &bytes.to_string(), workspace.to_str().context("probe directory is not UTF-8")?],
            "env": []
        }]
    })).await?;
    let session_id = session["sessionId"]
        .as_str()
        .context("missing ACP session ID")?;
    call("session/prompt", json!({
        "sessionId": session_id,
        "prompt": [{"type": "text", "text": "Call the schema_echo tool with {} once. If unavailable, reply unavailable. Do not use other tools."}]
    })).await?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [mode, case, bytes] if mode == "schema" => println!("{}", schema(case, bytes.parse()?)?),
        [mode, case, bytes, workspace] if mode == "stdio" => {
            mcp(
                case,
                bytes.parse()?,
                Path::new(workspace),
                io::stdin().lock(),
                io::stdout().lock(),
            )?;
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
    use super::*;

    #[tokio::test]
    async fn catalog_probe_round_trips_with_offline_acp_fixture() {
        let workspace = tempfile::tempdir().unwrap();
        let (driver, fake) = tokio::io::duplex(8192);
        let (driver_out, driver_in) = tokio::io::split(driver);
        let (fake_in, mut fake_out) = tokio::io::split(fake);
        let mut lines = BufReader::new(fake_in).lines();
        let fixture = async {
            let mut registration = Value::Null;
            for expected in ["initialize", "session/new", "session/prompt"] {
                let line = lines.next_line().await.unwrap().unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["method"], expected);
                let result = match expected {
                    "initialize" => json!({"protocolVersion": 1}),
                    "session/new" => {
                        registration = request["params"]["mcpServers"][0].clone();
                        assert_eq!(registration["name"], "schema-probe");
                        assert_eq!(registration["env"], json!([]));
                        assert_eq!(request["params"]["cwd"], json!(workspace.path()));
                        json!({"sessionId": "fixture"})
                    }
                    "session/prompt" => {
                        assert_eq!(request["params"]["sessionId"], "fixture");
                        assert!(
                            request["params"]["prompt"][0]["text"]
                                .as_str()
                                .unwrap()
                                .contains(TOOL)
                        );
                        let args = registration["args"].as_array().unwrap();
                        assert_eq!(args[0], "stdio");
                        let mut replies = Vec::new();
                        let frames = [
                            json!({"id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}),
                            json!({"id": 2, "method": "tools/list"}),
                            json!({"id": 3, "method": "tools/call", "params": {"name": TOOL, "arguments": {}}}),
                        ];
                        let input = frames
                            .iter()
                            .map(|frame| format!("{frame}\n"))
                            .collect::<String>();
                        mcp(
                            args[1].as_str().unwrap(),
                            args[2].as_str().unwrap().parse().unwrap(),
                            Path::new(args[3].as_str().unwrap()),
                            input.as_bytes(),
                            &mut replies,
                        )
                        .unwrap();
                        let replies: Vec<Value> = BufRead::lines(replies.as_slice())
                            .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
                            .collect();
                        assert_eq!(replies[0]["result"]["protocolVersion"], "2025-03-26");
                        let schema = &replies[1]["result"]["tools"][0]["inputSchema"];
                        assert_eq!(schema.to_string().len(), 1399);
                        assert!(schema.pointer("/properties/value/anyOf").is_some());
                        assert_eq!(
                            replies[2]["result"]["content"][0]["text"],
                            "probe acknowledgement"
                        );
                        json!({"stopReason": "end_turn"})
                    }
                    _ => unreachable!(),
                };
                let response = json!({"jsonrpc": "2.0", "id": request["id"], "result": result});
                fake_out
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
            }
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            let (result, ()) = tokio::join!(
                probe_session(driver_in, driver_out, workspace.path(), "any-of", 1399),
                fixture
            );
            result.unwrap();
        })
        .await
        .unwrap();
        assert!(workspace.path().join("listed").is_file());
        assert!(workspace.path().join("called").is_file());
    }

    #[test]
    fn catalog_probe_fixtures_hold_byte_size_constant_across_keywords() {
        for case in ["plain", "any-of", "union-type", "bare-items", "typed-items"] {
            let schema = super::schema(case, 1399).unwrap();
            assert_eq!(schema.to_string().len(), 1399, "{case}");
        }
    }
}

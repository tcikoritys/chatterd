use anyhow::{anyhow, Result};
use clap::Parser;
use serde_json::json;
use tokio::io::{stdout, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;

#[derive(Parser)]
#[command(name = "chatterctl", version, about = "CLI harness for chatterd JSON-RPC")]
struct Cli {
    #[arg(long, default_value = "127.0.0.1:9388")]
    addr: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match call(&cli.addr, "rpc.ping", None).await {
        Ok(resp) => println!("chatterctl connected to {} ({})", cli.addr, resp),
        Err(err) => println!("chatterctl not connected to {} ({err})", cli.addr),
    }
    println!("type `help` for commands, `quit` to exit");
    let mut events_task: Option<JoinHandle<()>> = None;

    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    while let Some(line) = lines.next_line().await? {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let mut parts: Vec<String> = line
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();
        if parts.is_empty() {
            continue;
        }
        if let Err(err) = handle_command(&cli.addr, &mut parts, &mut lines, &mut events_task).await
        {
            if err.to_string() == "quit" {
                break;
            }
            eprintln!("error: {err}");
        }
    }

    if let Some(task) = events_task {
        task.abort();
    }

    Ok(())
}

async fn handle_command(
    addr: &str,
    parts: &mut Vec<String>,
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    events_task: &mut Option<JoinHandle<()>>,
) -> Result<()> {
    if parts.is_empty() {
        return Ok(());
    }

    let cmd = resolve_prefix(&parts.remove(0), &[
        "ping",
        "version",
        "account",
        "events",
        "matrix",
        "room",
        "verification",
        "help",
        "quit",
        "exit",
    ])?;

    match cmd.as_str() {
        "help" => {
            print_help(parts);
            Ok(())
        }
        "quit" | "exit" => Err(anyhow!("quit")),
        "ping" => print_call(addr, "rpc.ping", None).await,
        "version" => print_call(addr, "rpc.version", None).await,
        "account" => handle_account(addr, parts, lines).await,
        "events" => handle_events(addr, parts, lines, events_task).await,
        "matrix" => handle_matrix(addr, parts, lines).await,
        "room" => handle_room(addr, parts, lines).await,
        "verification" => handle_verification(addr, parts, lines).await,
        _ => Ok(()),
    }
}

async fn handle_account(
    addr: &str,
    parts: &mut Vec<String>,
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
) -> Result<()> {
    if parts.is_empty() {
        print_help(&[String::from("account")]);
        return Ok(());
    }
    let sub = resolve_prefix(
        &parts.remove(0),
        &["add", "list", "login-start", "login-complete", "login-password", "session-restore"],
    )?;
    match sub.as_str() {
        "add" => {
            let homeserver = require_arg(parts, lines, "homeserver").await?;
            let login_method = take_flag_value(parts, "--login-method")
                .or_else(|| take_kv(parts, "login_method"));
            let params = json!({
                "homeserver": homeserver,
                "login_method": login_method,
            });
            print_call(addr, "account.add", Some(params)).await?;
        }
        "list" => {
            print_call(addr, "account.list", None).await?;
        }
        "login-start" => {
            let account_id = require_arg(parts, lines, "account_id").await?;
            let login_method = take_flag_value(parts, "--login-method")
                .or_else(|| take_kv(parts, "login_method"));
            let params = if let Some(method) = login_method {
                json!({ "account_id": account_id, "login_method": method })
            } else {
                json!({ "account_id": account_id })
            };
            print_call(addr, "account.login_start", Some(params)).await?;
        }
        "login-complete" => {
            let account_id = require_arg(parts, lines, "account_id").await?;
            let token = require_arg(parts, lines, "token").await?;
            let device_name = take_flag_value(parts, "--device-name")
                .or_else(|| take_kv(parts, "device_name"));
            let params = json!({
                "account_id": account_id,
                "token": token,
                "device_name": device_name,
            });
            print_call(addr, "account.login_complete", Some(params)).await?;
        }
        "login-password" => {
            let account_id = require_arg(parts, lines, "account_id").await?;
            let username = require_arg(parts, lines, "username").await?;
            let password = require_arg(parts, lines, "password").await?;
            let device_name = take_flag_value(parts, "--device-name")
                .or_else(|| take_kv(parts, "device_name"));
            let params = json!({
                "account_id": account_id,
                "username": username,
                "password": password,
                "device_name": device_name,
            });
            print_call(addr, "account.login_password", Some(params)).await?;
        }
        "session-restore" => {
            let account_id = require_arg(parts, lines, "account_id").await?;
            let params = json!({ "account_id": account_id });
            print_call(addr, "account.session_restore", Some(params)).await?;
        }
        _ => {}
    }
    Ok(())
}

async fn handle_events(
    addr: &str,
    parts: &mut Vec<String>,
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    events_task: &mut Option<JoinHandle<()>>,
) -> Result<()> {
    if parts.is_empty() {
        print_help(&[String::from("events")]);
        return Ok(());
    }
    let sub = resolve_prefix(&parts.remove(0), &["subscribe", "unsubscribe"])?;
    match sub.as_str() {
        "subscribe" => {
            let account_id = require_arg(parts, lines, "account_id").await?;
            let since = take_flag_value(parts, "--since").or_else(|| take_kv(parts, "since"));
            let snapshot = if has_flag(parts, "--no-snapshot") {
                false
            } else if has_flag(parts, "--snapshot") {
                true
            } else {
                take_kv(parts, "snapshot")
                    .map(|value| value != "false")
                    .unwrap_or(true)
            };
            let params = json!({
                "account_id": account_id,
                "since": since,
                "snapshot": snapshot,
            });
            if let Some(task) = events_task.take() {
                task.abort();
            }
            *events_task = Some(tokio::spawn(subscribe(addr.to_string(), params)));
        }
        "unsubscribe" => {
            if let Some(task) = events_task.take() {
                task.abort();
            }
            println!("events unsubscribed");
        }
        _ => {}
    }
    Ok(())
}

async fn handle_matrix(
    addr: &str,
    parts: &mut Vec<String>,
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
) -> Result<()> {
    if parts.is_empty() {
        print_help(&[String::from("matrix")]);
        return Ok(());
    }
    let sub = resolve_prefix(&parts.remove(0), &["login-flows", "capabilities"])?;
    match sub.as_str() {
        "login-flows" => {
            let homeserver = require_arg(parts, lines, "homeserver").await?;
            let params = json!({ "homeserver": homeserver });
            print_call(addr, "matrix.login_flows", Some(params)).await?;
        }
        "capabilities" => {
            let homeserver = require_arg(parts, lines, "homeserver").await?;
            let params = json!({ "homeserver": homeserver });
            print_call(addr, "matrix.capabilities", Some(params)).await?;
        }
        _ => {}
    }
    Ok(())
}

async fn handle_room(
    addr: &str,
    parts: &mut Vec<String>,
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
) -> Result<()> {
    if parts.is_empty() {
        print_help(&[String::from("room")]);
        return Ok(());
    }
    let sub = resolve_prefix(&parts.remove(0), &["list", "messages", "send"])?;
    match sub.as_str() {
        "list" => {
            let account_id = require_arg(parts, lines, "account_id").await?;
            let params = json!({ "account_id": account_id });
            print_call(addr, "room.list", Some(params)).await?;
        }
        "messages" => {
            let debug = has_flag(parts, "--debug");
            let limit = take_flag_value(parts, "--limit")
                .or_else(|| take_kv(parts, "limit"))
                .and_then(|value| value.parse::<usize>().ok());
            let from = take_flag_value(parts, "--from").or_else(|| take_kv(parts, "from"));
            let account_id = require_arg(parts, lines, "account_id").await?;
            let room_id = if !parts.is_empty() {
                parts.remove(0)
            } else {
                select_room(addr, lines, &account_id).await?
            };
            let mut params = serde_json::json!({
                "account_id": account_id,
                "room_id": room_id,
            });
            if let serde_json::Value::Object(ref mut map) = params {
                if let Some(limit) = limit {
                    map.insert("limit".to_string(), serde_json::Value::Number(limit.into()));
                }
                if let Some(from) = from {
                    map.insert("from".to_string(), serde_json::Value::String(from));
                }
            }
            let value = call_json(addr, "room.messages", Some(params)).await?;
            let result = value
                .get("result")
                .ok_or_else(|| anyhow!("room.messages: missing result"))?;
            let chunk = result
                .get("chunk")
                .and_then(|v| v.as_array())
                .ok_or_else(|| anyhow!("room.messages: missing chunk"))?;
            if chunk.is_empty() {
                println!("(no messages)");
            }
            for (idx, item) in chunk.iter().enumerate() {
                if debug && idx < 3 {
                    println!("debug event {}: {}", idx + 1, item);
                }
                match format_message_line(item) {
                    Some(line) => println!("{line}"),
                    None => {
                        let kind = event_type_hint(item).unwrap_or("unknown");
                        println!("<non-text event: {kind}>");
                    }
                }
            }
        }
        "send" => {
            let account_id = require_arg(parts, lines, "account_id").await?;
            let room_id = if !parts.is_empty() {
                parts.remove(0)
            } else {
                select_room(addr, lines, &account_id).await?
            };
            let body = if !parts.is_empty() {
                let text = parts.join(" ");
                parts.clear();
                text
            } else {
                require_arg(parts, lines, "body").await?
            };
            let txn_id = take_flag_value(parts, "--txn-id").or_else(|| take_kv(parts, "txn_id"));
            let params = json!({
                "account_id": account_id,
                "room_id": room_id,
                "body": body,
                "txn_id": txn_id,
            });
            print_call(addr, "room.send", Some(params)).await?;
        }
        _ => {}
    }
    Ok(())
}

async fn handle_verification(
    addr: &str,
    parts: &mut Vec<String>,
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
) -> Result<()> {
    if parts.is_empty() {
        print_help(&[String::from("verification")]);
        return Ok(());
    }
    let sub = resolve_prefix(&parts.remove(0), &["request", "accept", "confirm", "cancel"])?;
    match sub.as_str() {
        "request" => {
            let account_id = require_arg(parts, lines, "account_id").await?;
            let user_id = require_arg(parts, lines, "user_id").await?;
            let mut device_id = take_flag_value(parts, "--device-id")
                .or_else(|| take_kv(parts, "device_id"));
            if device_id.is_none() {
                device_id = select_device(addr, lines, &account_id, &user_id).await?;
            }
            let params = json!({
                "account_id": account_id,
                "user_id": user_id,
                "device_id": device_id,
            });
            print_call(addr, "matrix.verification.request", Some(params)).await?;
        }
        "accept" => {
            let account_id = require_arg(parts, lines, "account_id").await?;
            let user_id = require_arg(parts, lines, "user_id").await?;
            let flow_id = if parts.is_empty() {
                select_flow(addr, lines, &account_id, Some(&user_id)).await?
            } else {
                require_arg(parts, lines, "flow_id").await?
            };
            let params = json!({
                "account_id": account_id,
                "user_id": user_id,
                "flow_id": flow_id,
            });
            print_call(addr, "matrix.verification.accept", Some(params)).await?;
        }
        "confirm" => {
            let account_id = require_arg(parts, lines, "account_id").await?;
            let user_id = require_arg(parts, lines, "user_id").await?;
            let flow_id = if parts.is_empty() {
                select_flow(addr, lines, &account_id, Some(&user_id)).await?
            } else {
                require_arg(parts, lines, "flow_id").await?
            };
            let match_value = take_flag_value(parts, "--match")
                .or_else(|| take_kv(parts, "match"))
                .map(|value| value != "false")
                .unwrap_or(ask_yes_no(lines, "Do they match? (y/n)").await?);
            let params = json!({
                "account_id": account_id,
                "user_id": user_id,
                "flow_id": flow_id,
                "match": match_value,
            });
            print_call(addr, "matrix.verification.confirm", Some(params)).await?;
        }
        "cancel" => {
            let account_id = require_arg(parts, lines, "account_id").await?;
            let user_id = require_arg(parts, lines, "user_id").await?;
            let flow_id = if parts.is_empty() {
                select_flow(addr, lines, &account_id, Some(&user_id)).await?
            } else {
                require_arg(parts, lines, "flow_id").await?
            };
            let params = json!({
                "account_id": account_id,
                "user_id": user_id,
                "flow_id": flow_id,
            });
            print_call(addr, "matrix.verification.cancel", Some(params)).await?;
        }
        _ => {}
    }
    Ok(())
}

async fn call(addr: &str, method: &str, params: Option<serde_json::Value>) -> Result<String> {
    let mut stream = TcpStream::connect(addr).await?;
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let payload = serde_json::to_string(&request)?;
    stream.write_all(payload.as_bytes()).await?;
    stream.write_all(b"\n").await?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Err(anyhow!("no response"));
    }
    Ok(line.trim_end().to_string())
}

async fn call_json(
    addr: &str,
    method: &str,
    params: Option<serde_json::Value>,
) -> Result<serde_json::Value> {
    let resp = call(addr, method, params).await?;
    let value: serde_json::Value = serde_json::from_str(&resp)
        .map_err(|err| anyhow!("invalid json response: {err}"))?;
    Ok(value)
}

async fn subscribe(addr: String, params: serde_json::Value) {
    let mut stream = match TcpStream::connect(&addr).await {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("events.subscribe failed: {err}");
            return;
        }
    };
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "events.subscribe",
        "params": params,
    });
    let payload = match serde_json::to_string(&request) {
        Ok(payload) => payload,
        Err(err) => {
            eprintln!("events.subscribe encode failed: {err}");
            return;
        }
    };
    if let Err(err) = stream.write_all(payload.as_bytes()).await {
        eprintln!("events.subscribe write failed: {err}");
        return;
    }
    if let Err(err) = stream.write_all(b"\n").await {
        eprintln!("events.subscribe write failed: {err}");
        return;
    }

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        let n = match reader.read_line(&mut line).await {
            Ok(n) => n,
            Err(err) => {
                eprintln!("events.subscribe read failed: {err}");
                return;
            }
        };
        if n == 0 {
            return;
        }
        println!("event {}", line.trim_end());
    }
}

async fn print_call(
    addr: &str,
    method: &str,
    params: Option<serde_json::Value>,
) -> Result<()> {
    let resp = call(addr, method, params).await?;
    println!("{resp}");
    Ok(())
}

async fn require_arg(
    parts: &mut Vec<String>,
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    name: &str,
) -> Result<String> {
    if !parts.is_empty() {
        return Ok(parts.remove(0));
    }
    prompt(lines, name).await
}

fn take_kv(args: &mut Vec<String>, key: &str) -> Option<String> {
    if let Some(pos) = args.iter().position(|value| value == key) {
        if pos + 1 >= args.len() {
            return None;
        }
        args.remove(pos);
        Some(args.remove(pos))
    } else {
        None
    }
}

fn take_flag_value(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let pos = args.iter().position(|value| value == flag)?;
    if pos + 1 >= args.len() {
        return None;
    }
    args.remove(pos);
    Some(args.remove(pos))
}

fn has_flag(args: &mut Vec<String>, flag: &str) -> bool {
    if let Some(pos) = args.iter().position(|value| value == flag) {
        args.remove(pos);
        true
    } else {
        false
    }
}

async fn select_device(
    addr: &str,
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    account_id: &str,
    user_id: &str,
) -> Result<Option<String>> {
    let params = if is_self_user(user_id) {
        json!({
            "account_id": account_id,
        })
    } else {
        json!({
            "account_id": account_id,
            "user_id": user_id,
        })
    };
    let value = call_json(addr, "matrix.devices.list", Some(params)).await?;
    let list = value
        .get("result")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if list.is_empty() {
        return Err(anyhow!("no devices found"));
    }
    let labels: Vec<String> = list
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let device_id = item
                .get("device_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let name = item
                .get("display_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let verified = item
                .get("is_verified")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if name.is_empty() {
                format!("{idx}: {device_id}{}", if verified { " (verified)" } else { "" })
            } else {
                format!(
                    "{idx}: {name} ({device_id}){}",
                    if verified { " (verified)" } else { "" }
                )
            }
        })
        .collect();
    let index = prompt_select(lines, "Select device", &labels).await?;
    let device_id = list
        .get(index)
        .and_then(|item| item.get("device_id"))
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    Ok(device_id)
}

async fn select_room(
    addr: &str,
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    account_id: &str,
) -> Result<String> {
    let params = json!({ "account_id": account_id });
    let value = call_json(addr, "room.list", Some(params)).await?;
    let list = value
        .get("result")
        .and_then(|v| v.get("rooms"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if list.is_empty() {
        return Err(anyhow!("no rooms found"));
    }
    let labels: Vec<String> = list
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let room_id = item
                .get("room_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unnamed");
            format!("{idx}: {name} ({room_id})")
        })
        .collect();
    let index = prompt_select(lines, "Select room", &labels).await?;
    let room_id = list
        .get(index)
        .and_then(|item| item.get("room_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing room_id"))?;
    Ok(room_id.to_string())
}

async fn select_flow(
    addr: &str,
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    account_id: &str,
    user_id: Option<&str>,
) -> Result<String> {
    let params = match user_id {
        Some(value) if is_self_user(value) => json!({ "account_id": account_id }),
        Some(value) => json!({ "account_id": account_id, "user_id": value }),
        None => json!({ "account_id": account_id }),
    };
    let value = call_json(addr, "matrix.verification.list", Some(params)).await?;
    let list = value
        .get("result")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if list.is_empty() {
        return Err(anyhow!("no active verification flows"));
    }
    if list.len() == 1 {
        return list
            .get(0)
            .and_then(|item| item.get("flow_id"))
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
            .ok_or_else(|| anyhow!("missing flow id"));
    }
    let labels: Vec<String> = list
        .iter()
        .enumerate()
        .map(|(idx, item)| {
            let short_id = item
                .get("short_id")
                .and_then(|v| v.as_str())
                .unwrap_or("flow");
            let user = item
                .get("user_id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let device = item
                .get("device_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let stage = item
                .get("stage")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            if device.is_empty() {
                format!("{idx}: {short_id} {user} ({stage})")
            } else {
                format!("{idx}: {short_id} {user} {device} ({stage})")
            }
        })
        .collect();
    let index = prompt_select(lines, "Select verification flow", &labels).await?;
    let flow_id = list
        .get(index)
        .and_then(|item| item.get("flow_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("missing flow id"))?
        .to_string();
    Ok(flow_id)
}

async fn prompt_select(
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    label: &str,
    items: &[String],
) -> Result<usize> {
    for item in items {
        println!("{item}");
    }
    loop {
        let value = prompt(lines, label).await?;
        if let Ok(index) = value.parse::<usize>() {
            if index < items.len() {
                return Ok(index);
            }
        }
        println!("invalid selection");
    }
}

async fn ask_yes_no(
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    label: &str,
) -> Result<bool> {
    loop {
        let value = prompt(lines, label).await?;
        let value = value.to_lowercase();
        match value.as_str() {
            "y" | "yes" => return Ok(true),
            "n" | "no" => return Ok(false),
            _ => println!("please answer y/n"),
        }
    }
}

async fn prompt(
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    name: &str,
) -> Result<String> {
    loop {
        let mut out = stdout();
        out.write_all(format!("{name}: ").as_bytes()).await?;
        out.flush().await?;
        if let Some(line) = lines.next_line().await? {
            let value = line.trim().to_string();
            if !value.is_empty() {
                return Ok(value);
            }
        } else {
            return Err(anyhow!("missing {name}"));
        }
    }
}

fn is_self_user(value: &str) -> bool {
    value == "me" || value == "self"
}

fn resolve_prefix(input: &str, options: &[&str]) -> Result<String> {
    let matches: Vec<&str> = options.iter().copied().filter(|opt| opt.starts_with(input)).collect();
    match matches.len() {
        0 => Err(anyhow!("unknown command")),
        1 => Ok(matches[0].to_string()),
        _ => Err(anyhow!("ambiguous command: {}", matches.join(", "))),
    }
}

fn print_help(args: &[String]) {
    if args.is_empty() {
        println!("commands:");
        println!("  ping");
        println!("  version");
        println!("  account <subcommand>");
        println!("  events <subcommand>");
        println!("  matrix <subcommand>");
        println!("  room <subcommand>");
        println!("  verification <subcommand>");
        println!("  help [command]");
        println!("  quit");
        return;
    }
    match args[0].as_str() {
        "account" => {
            println!("account subcommands:");
            println!("  account add <homeserver> [--login-method sso|password]");
            println!("  account list");
            println!("  account login-start <account_id> [--login-method sso|password]");
            println!("  account login-complete <account_id> <token> [--device-name NAME]");
            println!("  account login-password <account_id> <username> <password> [--device-name NAME]");
            println!("  account session-restore <account_id>");
        }
        "events" => {
            println!("events subcommands:");
            println!("  events subscribe <account_id> [--since CURSOR] [--snapshot|--no-snapshot]");
            println!("  events unsubscribe");
        }
        "matrix" => {
            println!("matrix subcommands:");
            println!("  matrix login-flows <homeserver>");
            println!("  matrix capabilities <homeserver>");
        }
        "verification" => {
            println!("verification subcommands:");
            println!("  verification request <account_id> <user_id> [--device-id DEVICE]");
            println!("  verification accept <account_id> <user_id> <flow_id>");
            println!("  verification confirm <account_id> <user_id> <flow_id> [--match true|false]");
            println!("  verification cancel <account_id> <user_id> <flow_id>");
        }
        "room" => {
            println!("room subcommands:");
            println!("  room list <account_id>");
            println!("  room messages <account_id> <room_id> [--limit N] [--from TOKEN] [--debug]");
            println!("  room send <account_id> <room_id> <body...> [--txn-id ID]");
        }
        _ => println!("unknown help topic"),
    }
}

fn format_message_line(item: &serde_json::Value) -> Option<String> {
    let event = item.get("event").and_then(|v| v.as_object()).map(|_| item.get("event")).flatten().unwrap_or(item);
    let event_id = event.get("event_id").and_then(|v| v.as_str())?;
    let sender = event.get("sender").and_then(|v| v.as_str()).unwrap_or("unknown");
    let content = event.get("content");
    let msgtype = content
        .and_then(|v| v.get("msgtype"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let body = content
        .and_then(|v| v.get("body"))
        .and_then(|v| v.as_str());
    let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if msgtype == "m.text" || msgtype == "m.notice" || msgtype == "m.emote" {
        let body = body.unwrap_or("");
        return Some(format!("{sender} {event_id} {body}"));
    }
    if event_type == "m.room.message" {
        if let Some(body) = body {
            return Some(format!("{sender} {event_id} {body}"));
        }
    }
    None
}

fn event_type_hint(item: &serde_json::Value) -> Option<&str> {
    let event = item.get("event").and_then(|v| v.as_object()).map(|_| item.get("event")).flatten().unwrap_or(item);
    event.get("type").and_then(|v| v.as_str())
}

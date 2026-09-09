//! `codeapp run`: headless prompt execution.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use codeapp_client::{Client, ClientEvent, ClientOptions};
use codeapp_config::Paths;
use codeapp_types::{
    Attach, CallId, Effort, Event, Mode, ModelRoute, PermissionDecision, Request, ToolStatus,
    TranscriptKind, TurnStop,
};

pub struct RunOptions {
    pub prompt: String,
    pub json: bool,
    pub model: Option<ModelRoute>,
    pub effort: Option<Effort>,
    pub mode: Option<Mode>,
    pub cwd: PathBuf,
    pub socket: Option<PathBuf>,
    pub paths: Paths,
}

pub fn run(opts: RunOptions) -> anyhow::Result<()> {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("failed to create tokio runtime: {err}");
            std::process::exit(1);
        }
    };

    rt.block_on(async move {
        let timeout_duration = std::time::Duration::from_secs(30 * 60);
        match tokio::time::timeout(timeout_duration, run_headless(opts)).await {
            Ok(()) => {}
            Err(_) => {
                eprintln!("timed out waiting for turn completion (30 min limit)");
                std::process::exit(1);
            }
        }
    });

    Ok(())
}

async fn run_headless(opts: RunOptions) {
    let mut client_opts = ClientOptions::new(opts.paths.clone(), codeapp_config::APP_VERSION);
    client_opts.socket = opts.socket.clone();
    client_opts.spawn_daemon = true;

    let (client, mut events_rx) = match Client::connect(client_opts).await {
        Ok(res) => res,
        Err(err) => {
            eprintln!("connection error: {err}");
            std::process::exit(2);
        }
    };

    let attach = Attach::New {
        cwd: opts.cwd,
        model: opts.model,
        effort: opts.effort,
        mode: opts.mode,
    };

    if let Err(err) = client.attach(attach).await {
        eprintln!("attach error: {err}");
        std::process::exit(2);
    }

    let user_msg = Request::UserMessage {
        text: opts.prompt.clone(),
    };

    if let Err(err) = client.request(user_msg).await {
        eprintln!("message send error: {err}");
        std::process::exit(2);
    }

    let mut tool_statuses: HashMap<CallId, ToolStatus> = HashMap::new();

    loop {
        let client_event = match events_rx.recv().await {
            Some(ev) => ev,
            None => {
                eprintln!("event stream closed unexpectedly");
                std::process::exit(2);
            }
        };

        match client_event {
            ClientEvent::Connected { .. } | ClientEvent::Snapshot(_) => {}
            ClientEvent::Disconnected { reason } => {
                eprintln!("disconnected from daemon: {reason}");
                std::process::exit(2);
            }
            ClientEvent::Reconnecting { .. } => {}
            ClientEvent::Event { seq, event } => {
                if opts.json {
                    let line = match serde_json::to_string(&serde_json::json!({
                        "seq": seq,
                        "event": event,
                    })) {
                        Ok(l) => l,
                        Err(err) => {
                            eprintln!("serialization error: {err}");
                            std::process::exit(1);
                        }
                    };
                    println!("{line}");
                    let _ = std::io::stdout().flush();
                }

                match &event {
                    Event::TextDelta { agent, text, .. } => {
                        if !opts.json && agent.is_main() {
                            print!("{text}");
                            let _ = std::io::stdout().flush();
                        }
                    }
                    Event::ItemAdded(item) | Event::ItemUpdated(item) => {
                        if !opts.json
                            && let TranscriptKind::ToolCall {
                                call_id,
                                title,
                                status,
                                ..
                            } = &item.kind
                            && tool_statuses.get(call_id) != Some(status)
                        {
                            tool_statuses.insert(call_id.clone(), *status);
                            eprintln!("[tool] {title} … {}", tool_status_str(*status));
                            let _ = std::io::stderr().flush();
                        }
                    }
                    Event::PermissionRequested(req) => {
                        eprintln!(
                            "auto-denying permission request: {} ({})",
                            req.title, req.tool
                        );
                        let _ = std::io::stderr().flush();
                        let reply = Request::PermissionReply {
                            permission: req.id.clone(),
                            decision: PermissionDecision::Deny,
                        };
                        let _ = client.request(reply).await;
                    }
                    Event::TurnEnded { agent, stop, .. } if agent.is_main() => {
                        if !opts.json {
                            println!();
                            let _ = std::io::stdout().flush();
                        }
                        match stop {
                            TurnStop::Completed => {
                                std::process::exit(0);
                            }
                            TurnStop::Interrupted => {
                                eprintln!("turn interrupted");
                                std::process::exit(1);
                            }
                            TurnStop::Failed { message } => {
                                eprintln!("turn failed: {message}");
                                std::process::exit(1);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn tool_status_str(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Running => "running",
        ToolStatus::Ok => "ok",
        ToolStatus::Error => "error",
        ToolStatus::Backgrounded => "backgrounded",
        ToolStatus::Denied => "denied",
    }
}

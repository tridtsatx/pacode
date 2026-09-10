//! `pacode login`: sign in to a model provider.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::{Context, bail};
use pacode_client::{Client, ClientEvent, ClientOptions};
use pacode_config::Paths;
use pacode_types::{AuthState, Event, LoginStage, ProviderAuthInfo, Reply, Request};

pub fn run(
    provider: Option<String>,
    list: bool,
    socket: Option<PathBuf>,
    paths: Paths,
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create tokio runtime")?;

    rt.block_on(async move {
        let mut client_opts = ClientOptions::new(paths, pacode_config::APP_VERSION);
        client_opts.socket = socket;
        client_opts.spawn_daemon = true;

        let (client, mut events_rx) = Client::connect(client_opts)
            .await
            .context("failed to connect to daemon")?;

        if list {
            return list_providers(&client).await;
        }

        let provider_id = match provider {
            Some(id) if !id.trim().is_empty() => id.trim().to_string(),
            _ => {
                let providers = fetch_providers(&client).await?;
                if providers.is_empty() {
                    println!("No providers available");
                    return Ok(());
                }
                println!("Select a provider to sign in:");
                for (i, p) in providers.iter().enumerate() {
                    let status = format_status(&p.state);
                    println!(
                        "  {}. {} ({}) [{}] - {status}",
                        i + 1,
                        p.display_name,
                        p.id,
                        p.auth_kind
                    );
                }
                print!("Enter number (1-{}): ", providers.len());
                std::io::stdout().flush().context("flush stdout")?;

                let mut input = String::new();
                let stdin = std::io::stdin();
                stdin.lock().read_line(&mut input).context("read stdin")?;
                let trimmed = input.trim();
                let chosen_idx = match trimmed.parse::<usize>() {
                    Ok(n) if n >= 1 && n <= providers.len() => n - 1,
                    _ => {
                        if let Some(pos) = providers
                            .iter()
                            .position(|p| p.id.eq_ignore_ascii_case(trimmed))
                        {
                            pos
                        } else {
                            bail!("invalid selection: {trimmed}");
                        }
                    }
                };
                providers[chosen_idx].id.clone()
            }
        };

        // Send login request
        let reply = client
            .request(Request::Login {
                provider: provider_id.clone(),
            })
            .await
            .context("failed to send login request")?;

        match reply {
            Reply::Ok => {}
            Reply::Error { message } => bail!("daemon error: {message}"),
            other => bail!("unexpected reply: {other:?}"),
        }

        // Await login progress events
        while let Some(client_ev) = events_rx.recv().await {
            match client_ev {
                ClientEvent::Event {
                    event: Event::LoginProgress { provider: p, stage },
                    ..
                } if p == provider_id => match stage {
                    LoginStage::OpenUrl { url, opened } => {
                        if opened {
                            println!("Opened browser to: {url}");
                        } else {
                            println!("Open this URL in your browser to sign in:\n{url}");
                        }
                    }
                    LoginStage::Waiting => {
                        println!("Waiting for authorization...");
                    }
                    LoginStage::Exchanging => {
                        println!("Exchanging authorization code...");
                    }
                    LoginStage::Done { label } => {
                        println!("Successfully signed in to {provider_id} ({label})");
                        return Ok(());
                    }
                    LoginStage::Failed { message } => {
                        bail!("Login to {provider_id} failed: {message}");
                    }
                },
                ClientEvent::Disconnected { reason } => {
                    bail!("Disconnected from daemon: {reason}");
                }
                _ => {}
            }
        }

        bail!("Daemon connection closed unexpectedly");
    })
}

async fn fetch_providers(client: &Client) -> anyhow::Result<Vec<ProviderAuthInfo>> {
    let reply = client
        .request(Request::ListAuth)
        .await
        .context("failed to list auth providers")?;

    match reply {
        Reply::AuthStatus { providers } => Ok(providers),
        Reply::Error { message } => bail!("daemon error: {message}"),
        other => bail!("unexpected reply: {other:?}"),
    }
}

async fn list_providers(client: &Client) -> anyhow::Result<()> {
    let providers = fetch_providers(client).await?;
    if providers.is_empty() {
        println!("No providers available");
        return Ok(());
    }

    println!("{:<16}  {:<24}  {:<10}  status", "id", "name", "kind");
    for p in providers {
        let status = format_status(&p.state);
        let status_str = if let Some(ref active) = p.active {
            format!("{status} (active: {active})")
        } else {
            status
        };
        println!(
            "{:<16}  {:<24}  {:<10}  {}",
            p.id, p.display_name, p.auth_kind, status_str
        );
    }
    Ok(())
}

fn format_status(state: &AuthState) -> String {
    match state {
        AuthState::Configured => "configured".to_string(),
        AuthState::NeedsAttention { reason } => format!("needs attention ({reason})"),
        AuthState::NotConfigured => "not configured".to_string(),
    }
}

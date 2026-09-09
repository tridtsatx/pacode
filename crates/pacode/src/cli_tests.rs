#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser;
    use pacode_types::{Attach, Effort, Mode, ModelRoute, SessionId};

    use crate::cli::{
        Cli, Command, DaemonAction, SessionsAction, build_attach, parse_effort_override,
        parse_mode_override, parse_model_override,
    };

    #[test]
    fn parse_default_empty() {
        let cli = Cli::try_parse_from(["pacode"]).expect("should parse empty args");
        assert!(cli.prompt.is_none());
        assert!(cli.session.is_none());
        assert!(cli.model.is_none());
        assert!(cli.effort.is_none());
        assert!(cli.mode.is_none());
        assert!(cli.dir.is_none());
        assert!(cli.socket.is_none());
        assert!(cli.command.is_none());
    }

    #[test]
    fn parse_default_with_prompt() {
        let cli = Cli::try_parse_from(["pacode", "fix this bug"]).expect("should parse prompt");
        assert_eq!(cli.prompt.as_deref(), Some("fix this bug"));
        assert!(cli.command.is_none());
    }

    #[test]
    fn parse_default_with_flags() {
        let cli = Cli::try_parse_from([
            "pacode",
            "explain code",
            "--model",
            "bubna/gemini-3.8-flash",
            "--effort",
            "high",
            "--mode",
            "bypass",
            "-C",
            "/tmp",
            "--socket",
            "/tmp/test.sock",
        ])
        .expect("should parse flags");
        assert_eq!(cli.prompt.as_deref(), Some("explain code"));
        assert_eq!(cli.model.as_deref(), Some("bubna/gemini-3.8-flash"));
        assert_eq!(cli.effort.as_deref(), Some("high"));
        assert_eq!(cli.mode.as_deref(), Some("bypass"));
        assert_eq!(cli.dir, Some(PathBuf::from("/tmp")));
        assert_eq!(cli.socket, Some(PathBuf::from("/tmp/test.sock")));
    }

    #[test]
    fn parse_default_with_session_short() {
        let cli = Cli::try_parse_from(["pacode", "-s", "pacode-12345678-abcd"])
            .expect("should parse session short");
        assert_eq!(cli.session.as_deref(), Some("pacode-12345678-abcd"));
        assert!(cli.prompt.is_none());
    }

    #[test]
    fn parse_default_with_session_long() {
        let cli = Cli::try_parse_from(["pacode", "--session", "pacode-12345678-abcd"])
            .expect("should parse session long");
        assert_eq!(cli.session.as_deref(), Some("pacode-12345678-abcd"));
        assert!(cli.prompt.is_none());
    }

    #[test]
    fn parse_default_with_resume() {
        let cli = Cli::try_parse_from(["pacode", "--resume", "pacode-12345678-abcd"])
            .expect("should parse resume alias");
        assert_eq!(cli.session.as_deref(), Some("pacode-12345678-abcd"));
        assert!(cli.prompt.is_none());
    }

    #[test]
    fn help_render_contains_tagline_and_mascot() {
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        let help = cmd.render_help().to_string();
        assert!(help.contains("pacode v"));
        assert!(help.contains("a coding agent that eats your backlog"));
        assert!(help.contains("▄▄▄▄▄"));
        assert!(help.contains("-s, --session <ID>"));
        assert!(!help.contains("--resume"));
        assert!(help.contains("serve"));
        assert!(help.contains("run"));
        assert!(help.contains("sessions"));
        assert!(help.contains("daemon"));
    }

    #[test]
    fn parse_serve_shapes() {
        let cli = Cli::try_parse_from(["pacode", "serve"]).expect("serve plain");
        match cli.command {
            Some(Command::Serve { detach, socket }) => {
                assert!(!detach);
                assert!(socket.is_none());
            }
            other => panic!("expected Serve, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["pacode", "serve", "--detach"]).expect("serve detach");
        match cli.command {
            Some(Command::Serve { detach, socket }) => {
                assert!(detach);
                assert!(socket.is_none());
            }
            other => panic!("expected Serve, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["pacode", "serve", "--socket", "/tmp/d.sock"])
            .expect("serve socket");
        match cli.command {
            Some(Command::Serve { detach, socket }) => {
                assert!(!detach);
                assert_eq!(socket, Some(PathBuf::from("/tmp/d.sock")));
            }
            other => panic!("expected Serve, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["pacode", "serve", "--detach", "--socket", "/tmp/d.sock"])
            .expect("serve detach socket");
        match cli.command {
            Some(Command::Serve { detach, socket }) => {
                assert!(detach);
                assert_eq!(socket, Some(PathBuf::from("/tmp/d.sock")));
            }
            other => panic!("expected Serve, got {other:?}"),
        }
    }

    #[test]
    fn parse_run_shapes() {
        let cli = Cli::try_parse_from(["pacode", "run", "do task"]).expect("run basic");
        match cli.command {
            Some(Command::Run {
                prompt,
                json,
                model,
                effort,
                mode,
                dir,
            }) => {
                assert_eq!(prompt, "do task");
                assert!(!json);
                assert!(model.is_none());
                assert!(effort.is_none());
                assert!(mode.is_none());
                assert!(dir.is_none());
            }
            other => panic!("expected Run, got {other:?}"),
        }

        let cli = Cli::try_parse_from([
            "pacode",
            "run",
            "do task",
            "--json",
            "--model",
            "bubna/gemini-3.8-flash",
            "--effort",
            "max",
            "--mode",
            "auto",
            "-C",
            "/var/tmp",
        ])
        .expect("run with all flags");
        match cli.command {
            Some(Command::Run {
                prompt,
                json,
                model,
                effort,
                mode,
                dir,
            }) => {
                assert_eq!(prompt, "do task");
                assert!(json);
                assert_eq!(model.as_deref(), Some("bubna/gemini-3.8-flash"));
                assert_eq!(effort.as_deref(), Some("max"));
                assert_eq!(mode.as_deref(), Some("auto"));
                assert_eq!(dir, Some(PathBuf::from("/var/tmp")));
            }
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn parse_sessions_shapes() {
        let cli = Cli::try_parse_from(["pacode", "sessions"]).expect("sessions empty");
        match cli.command {
            Some(Command::Sessions { action }) => {
                assert!(action.is_none());
            }
            other => panic!("expected Sessions, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["pacode", "sessions", "list"]).expect("sessions list");
        match cli.command {
            Some(Command::Sessions {
                action: Some(SessionsAction::List { limit }),
            }) => {
                assert_eq!(limit, 20);
            }
            other => panic!("expected Sessions List, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["pacode", "sessions", "list", "--limit", "50"])
            .expect("sessions list limit");
        match cli.command {
            Some(Command::Sessions {
                action: Some(SessionsAction::List { limit }),
            }) => {
                assert_eq!(limit, 50);
            }
            other => panic!("expected Sessions List, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["pacode", "sessions", "delete", "ses_abc123"])
            .expect("sessions delete");
        match cli.command {
            Some(Command::Sessions {
                action: Some(SessionsAction::Delete { id }),
            }) => {
                assert_eq!(id, "ses_abc123");
            }
            other => panic!("expected Sessions Delete, got {other:?}"),
        }
    }

    #[test]
    fn parse_daemon_shapes() {
        let cli = Cli::try_parse_from(["pacode", "daemon", "status"]).expect("daemon status");
        match cli.command {
            Some(Command::Daemon {
                action: DaemonAction::Status,
            }) => {}
            other => panic!("expected Daemon Status, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["pacode", "daemon", "stop"]).expect("daemon stop");
        match cli.command {
            Some(Command::Daemon {
                action: DaemonAction::Stop { force },
            }) => {
                assert!(!force);
            }
            other => panic!("expected Daemon Stop, got {other:?}"),
        }

        let cli = Cli::try_parse_from(["pacode", "daemon", "stop", "--force"])
            .expect("daemon stop force");
        match cli.command {
            Some(Command::Daemon {
                action: DaemonAction::Stop { force },
            }) => {
                assert!(force);
            }
            other => panic!("expected Daemon Stop, got {other:?}"),
        }
    }

    #[test]
    fn parse_invalid_flag() {
        assert!(Cli::try_parse_from(["pacode", "--unknown-flag"]).is_err());
    }

    #[test]
    fn build_attach_resume() {
        let cwd = PathBuf::from("/workspace");
        let attach = build_attach(
            Some("pacode-99990000-1111".to_string()),
            cwd.clone(),
            Some(ModelRoute::new("p", "m")),
            Some(Effort::High),
            Some(Mode::Auto),
        );
        assert_eq!(
            attach,
            Attach::Resume {
                session: SessionId::new("pacode-99990000-1111"),
            }
        );
    }

    #[test]
    fn build_attach_new() {
        let cwd = PathBuf::from("/workspace");
        let route = ModelRoute::new("bubna", "gemini-3.8-flash");
        let attach = build_attach(
            None,
            cwd.clone(),
            Some(route.clone()),
            Some(Effort::Max),
            Some(Mode::Bypass),
        );
        assert_eq!(
            attach,
            Attach::New {
                cwd: cwd.clone(),
                model: Some(route),
                effort: Some(Effort::Max),
                mode: Some(Mode::Bypass),
            }
        );
    }

    #[test]
    fn build_attach_empty_resume_falls_back_to_new() {
        let cwd = PathBuf::from("/workspace");
        let attach = build_attach(Some("   ".to_string()), cwd.clone(), None, None, None);
        assert_eq!(
            attach,
            Attach::New {
                cwd,
                model: None,
                effort: None,
                mode: None,
            }
        );
    }

    #[test]
    fn parse_overrides() {
        let config = pacode_types::Config::default();

        assert!(parse_model_override(None, &config).unwrap().is_none());
        let route = parse_model_override(Some("bubna/gemini"), &config)
            .unwrap()
            .unwrap();
        assert_eq!(route.provider, "bubna");
        assert_eq!(route.model, "gemini");

        assert!(parse_effort_override(None).unwrap().is_none());
        assert_eq!(
            parse_effort_override(Some("high")).unwrap(),
            Some(Effort::High)
        );
        assert_eq!(
            parse_effort_override(Some("max")).unwrap(),
            Some(Effort::Max)
        );
        assert!(parse_effort_override(Some("invalid")).is_err());

        assert!(parse_mode_override(None).unwrap().is_none());
        assert_eq!(
            parse_mode_override(Some("bypass")).unwrap(),
            Some(Mode::Bypass)
        );
        assert_eq!(parse_mode_override(Some("auto")).unwrap(), Some(Mode::Auto));
        assert!(parse_mode_override(Some("invalid")).is_err());
    }
}

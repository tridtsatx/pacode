use pacode_tools::builtin::builtin_tools;
use pacode_tools::{Tool, ToolKind};
use pacode_types::{Mode, RiskLevel, WebConfig};

use crate::permissions::{GateDecision, gate};

#[test]
fn test_every_builtin_tool_reports_expected_kind() {
    let tools = builtin_tools(&WebConfig::default());

    // Spec §6.3 & §7 kind mapping
    let expected: &[(&str, ToolKind)] = &[
        ("read", ToolKind::ReadOnly),
        ("write", ToolKind::Edit),
        ("edit", ToolKind::Edit),
        ("multi_edit", ToolKind::Edit),
        ("bash", ToolKind::Exec),
        ("bg", ToolKind::Control),
        ("grep", ToolKind::ReadOnly),
        ("glob", ToolKind::ReadOnly),
        ("ls", ToolKind::ReadOnly),
        ("plan", ToolKind::Control),
        ("agent", ToolKind::Control),
        ("webfetch", ToolKind::Network),
        ("websearch", ToolKind::Network),
        ("report_status", ToolKind::Control),
        ("memory_write", ToolKind::Edit),
        ("memory_read", ToolKind::ReadOnly),
    ];

    for (name, kind) in expected {
        let tool = tools
            .get(name)
            .unwrap_or_else(|| panic!("expected builtin tool {name} to be registered"));
        assert_eq!(
            tool.kind(),
            *kind,
            "tool {name} reported kind {:?}, expected {:?}",
            tool.kind(),
            kind
        );
    }
}

#[test]
fn test_permission_matrix_decisions_for_known_tools_table_driven() {
    // Table driven test proving permission matrix decisions for known tools are unchanged
    struct Row {
        tool_name: &'static str,
        kind: ToolKind,
        risk: Option<RiskLevel>,
        build_expected: GateDecision,
        auto_expected: GateDecision,
        plan_expected: GateDecision,
        bypass_expected: GateDecision,
    }

    let rows = vec![
        // Read-only tools: read, ls, grep, glob, memory_read
        Row {
            tool_name: "read",
            kind: ToolKind::ReadOnly,
            risk: None,
            build_expected: GateDecision::Allow,
            auto_expected: GateDecision::Allow,
            plan_expected: GateDecision::Allow,
            bypass_expected: GateDecision::Allow,
        },
        Row {
            tool_name: "ls",
            kind: ToolKind::ReadOnly,
            risk: None,
            build_expected: GateDecision::Allow,
            auto_expected: GateDecision::Allow,
            plan_expected: GateDecision::Allow,
            bypass_expected: GateDecision::Allow,
        },
        Row {
            tool_name: "grep",
            kind: ToolKind::ReadOnly,
            risk: None,
            build_expected: GateDecision::Allow,
            auto_expected: GateDecision::Allow,
            plan_expected: GateDecision::Allow,
            bypass_expected: GateDecision::Allow,
        },
        Row {
            tool_name: "glob",
            kind: ToolKind::ReadOnly,
            risk: None,
            build_expected: GateDecision::Allow,
            auto_expected: GateDecision::Allow,
            plan_expected: GateDecision::Allow,
            bypass_expected: GateDecision::Allow,
        },
        // Edit tools: write, edit, multi_edit, memory_write
        Row {
            tool_name: "write",
            kind: ToolKind::Edit,
            risk: None,
            build_expected: GateDecision::Ask,
            auto_expected: GateDecision::Allow,
            plan_expected: GateDecision::Deny("file edits are not permitted in plan mode".into()),
            bypass_expected: GateDecision::Allow,
        },
        Row {
            tool_name: "edit",
            kind: ToolKind::Edit,
            risk: None,
            build_expected: GateDecision::Ask,
            auto_expected: GateDecision::Allow,
            plan_expected: GateDecision::Deny("file edits are not permitted in plan mode".into()),
            bypass_expected: GateDecision::Allow,
        },
        Row {
            tool_name: "multi_edit",
            kind: ToolKind::Edit,
            risk: None,
            build_expected: GateDecision::Ask,
            auto_expected: GateDecision::Allow,
            plan_expected: GateDecision::Deny("file edits are not permitted in plan mode".into()),
            bypass_expected: GateDecision::Allow,
        },
        // Exec: bash with various risks
        Row {
            tool_name: "bash (safe)",
            kind: ToolKind::Exec,
            risk: Some(RiskLevel::Safe),
            build_expected: GateDecision::Allow,
            auto_expected: GateDecision::Allow,
            plan_expected: GateDecision::Allow,
            bypass_expected: GateDecision::Allow,
        },
        Row {
            tool_name: "bash (low)",
            kind: ToolKind::Exec,
            risk: Some(RiskLevel::Low),
            build_expected: GateDecision::Ask,
            auto_expected: GateDecision::Allow,
            plan_expected: GateDecision::Deny(
                "exec operations are not permitted in plan mode".into(),
            ),
            bypass_expected: GateDecision::Allow,
        },
        Row {
            tool_name: "bash (confirm)",
            kind: ToolKind::Exec,
            risk: Some(RiskLevel::Confirm),
            build_expected: GateDecision::Ask,
            auto_expected: GateDecision::Ask,
            plan_expected: GateDecision::Deny(
                "exec operations are not permitted in plan mode".into(),
            ),
            bypass_expected: GateDecision::Allow,
        },
        Row {
            tool_name: "bash (catastrophic)",
            kind: ToolKind::Exec,
            risk: Some(RiskLevel::Catastrophic),
            build_expected: GateDecision::Deny(
                "catastrophic operations are denied by policy".into(),
            ),
            auto_expected: GateDecision::Deny(
                "catastrophic operations are denied by policy".into(),
            ),
            plan_expected: GateDecision::Deny(
                "catastrophic operations are denied by policy".into(),
            ),
            bypass_expected: GateDecision::Deny(
                "catastrophic operations are denied by policy".into(),
            ),
        },
        // Control tools: plan, agent, bg
        Row {
            tool_name: "plan",
            kind: ToolKind::Control,
            risk: None,
            build_expected: GateDecision::Allow,
            auto_expected: GateDecision::Allow,
            plan_expected: GateDecision::Allow,
            bypass_expected: GateDecision::Allow,
        },
        Row {
            tool_name: "agent",
            kind: ToolKind::Control,
            risk: None,
            build_expected: GateDecision::Allow,
            auto_expected: GateDecision::Allow,
            plan_expected: GateDecision::Allow,
            bypass_expected: GateDecision::Allow,
        },
        Row {
            tool_name: "bg",
            kind: ToolKind::Control,
            risk: None,
            build_expected: GateDecision::Allow,
            auto_expected: GateDecision::Allow,
            plan_expected: GateDecision::Allow,
            bypass_expected: GateDecision::Allow,
        },
        // Network tools: webfetch, websearch
        Row {
            tool_name: "webfetch",
            kind: ToolKind::Network,
            risk: None,
            build_expected: GateDecision::Allow,
            auto_expected: GateDecision::Allow,
            plan_expected: GateDecision::Allow,
            bypass_expected: GateDecision::Allow,
        },
        Row {
            tool_name: "websearch",
            kind: ToolKind::Network,
            risk: None,
            build_expected: GateDecision::Allow,
            auto_expected: GateDecision::Allow,
            plan_expected: GateDecision::Allow,
            bypass_expected: GateDecision::Allow,
        },
    ];

    for r in rows {
        assert_eq!(
            gate(Mode::Build, r.kind, r.risk, false),
            r.build_expected,
            "Build mode mismatch for {}",
            r.tool_name
        );
        assert_eq!(
            gate(Mode::Auto, r.kind, r.risk, false),
            r.auto_expected,
            "Auto mode mismatch for {}",
            r.tool_name
        );
        assert_eq!(
            gate(Mode::Plan, r.kind, r.risk, false),
            r.plan_expected,
            "Plan mode mismatch for {}",
            r.tool_name
        );
        assert_eq!(
            gate(Mode::Bypass, r.kind, r.risk, false),
            r.bypass_expected,
            "Bypass mode mismatch for {}",
            r.tool_name
        );
    }
}

#[test]
fn catastrophic_override_is_bypass_only() {
    for mode in [Mode::Build, Mode::Auto, Mode::Plan] {
        assert!(matches!(
            gate(mode, ToolKind::Exec, Some(RiskLevel::Catastrophic), true),
            GateDecision::Deny(_)
        ));
    }
    assert_eq!(
        gate(
            Mode::Bypass,
            ToolKind::Exec,
            Some(RiskLevel::Catastrophic),
            true
        ),
        GateDecision::Allow
    );
}

#[test]
fn test_unexpected_title_is_not_auto_allowed() {
    // An unknown or unclassifiable tool must fail closed:
    // treated as ToolKind::Exec (Confirm), NOT ToolKind::ReadOnly!
    let fail_closed_kind = ToolKind::Exec;
    let unclassified_risk = None; // defaults to Confirm in gate()

    // Must NOT be auto-allowed in Build mode (requires user confirmation)
    let decision_build = gate(Mode::Build, fail_closed_kind, unclassified_risk, false);
    assert_eq!(
        decision_build,
        GateDecision::Ask,
        "unexpected tool title must fail closed and Ask in Build mode"
    );

    // Must NOT be auto-allowed in Auto mode (requires user confirmation)
    let decision_auto = gate(Mode::Auto, fail_closed_kind, unclassified_risk, false);
    assert_eq!(
        decision_auto,
        GateDecision::Ask,
        "unexpected tool title must fail closed and Ask in Auto mode"
    );

    // Must be denied in Plan mode
    let decision_plan = gate(Mode::Plan, fail_closed_kind, unclassified_risk, false);
    assert!(
        matches!(decision_plan, GateDecision::Deny(_)),
        "unexpected tool title must fail closed and Deny in Plan mode"
    );
}

#[test]
fn test_mcp_provided_tool_is_not_auto_allowed() {
    use pacode_mcp::McpToolInfo;

    // MCP tool wrapper defaults to ToolKind::Exec to fail closed
    let mcp_info = McpToolInfo {
        name: "custom_query".to_string(),
        description: "Executes custom remote query".to_string(),
        input_schema: serde_json::json!({}),
    };
    let pool = pacode_mcp::McpPool::new(Default::default(), None, None);
    let mcp_tool = pacode_tools::builtin::mcp::McpTool::new(pool, "remote_server".into(), mcp_info);

    assert_eq!(
        mcp_tool.kind(),
        ToolKind::Exec,
        "MCP tool must report Exec kind by default"
    );

    // Verify MCP tool is NOT auto-allowed in Build mode
    let decision_build = gate(Mode::Build, mcp_tool.kind(), None, false);
    assert_eq!(
        decision_build,
        GateDecision::Ask,
        "MCP tool must not be auto-allowed in Build mode"
    );

    // Verify MCP tool is NOT auto-allowed in Auto mode
    let decision_auto = gate(Mode::Auto, mcp_tool.kind(), None, false);
    assert_eq!(
        decision_auto,
        GateDecision::Ask,
        "MCP tool must not be auto-allowed in Auto mode"
    );

    // Verify MCP tool is Denied in Plan mode
    let decision_plan = gate(Mode::Plan, mcp_tool.kind(), None, false);
    assert!(
        matches!(decision_plan, GateDecision::Deny(_)),
        "MCP tool must be denied in Plan mode"
    );
}

// ---- ZDR isolation (spec §3): while ZDR is active the host must deny
// network tools, MCP (`server__tool`) names and subagent spawning; local
// read/fs tools still flow through the normal gate. ----

mod zdr {
    use std::collections::{BTreeMap, HashMap, VecDeque};
    use std::sync::{Arc, RwLock};

    use pacode_exec::TaskSpec;
    use pacode_tools::host::PermissionDraft;
    use pacode_tools::{ToolHost, ToolKind};
    use pacode_types::{
        AgentId, CallId, Mode, ModelFlags, ModelRoute, PermissionDecision, SessionMeta, ZdrMode,
    };

    use crate::host::{SessionHost, permission_target};
    use crate::session::Session;

    /// Smallest live `Session` for host-level checks (in-memory store, temp cwd).
    fn zdr_session(zdr_mode: ZdrMode) -> (tempfile::TempDir, Arc<Session>) {
        new_session(zdr_mode)
    }

    fn zdr_session_with_main_agent(zdr_mode: ZdrMode) -> (tempfile::TempDir, Arc<Session>) {
        let (temp, session) = new_session(zdr_mode);
        let now = pacode_types::now_ms();
        let info = pacode_types::AgentInfo {
            id: AgentId::main(),
            name: "main".to_string(),
            kind: pacode_types::AgentKind::Main,
            status: pacode_types::AgentStatus::Idle,
            activity: None,
            started_at_ms: now,
            finished_at_ms: None,
            tokens_in: 0,
            tokens_out: 0,
            model: session.meta().model.clone(),
            effort: pacode_types::Effort::Medium,
            parent: None,
            summary: None,
            error: None,
        };
        let agent = Arc::new(crate::agent::Agent::new(
            AgentId::main(),
            info,
            crate::agent::History::default(),
            pacode_tools::ToolRegistry::new(),
            200,
            None,
        ));
        session
            .agents
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(AgentId::main(), agent);
        (temp, session)
    }

    fn new_session(zdr_mode: ZdrMode) -> (tempfile::TempDir, Arc<Session>) {
        let temp = tempfile::tempdir().unwrap();
        let id = pacode_types::SessionId::generate();
        let now = pacode_types::now_ms();
        let route = ModelRoute {
            provider: "mock".into(),
            model: "mock-model".into(),
        };
        let meta = SessionMeta {
            id: id.clone(),
            name: None,
            cwd: temp.path().to_path_buf(),
            git_branch: None,
            created_at_ms: now,
            updated_at_ms: now,
            model: route,
            effort: pacode_types::Effort::Medium,
            mode: Mode::Build,
            first_prompt: None,
            zdr: false,
        };
        let tools = pacode_tools::ToolRegistry::new();
        let config = pacode_types::Config {
            zdr: zdr_mode,
            ..pacode_types::Config::default()
        };
        let session = Arc::new(Session {
            id,
            meta: RwLock::new(meta),
            agents: RwLock::new(BTreeMap::new()),
            agent_aliases: std::sync::Mutex::new(HashMap::new()),
            plan: RwLock::new(pacode_types::Plan::default()),
            usage: RwLock::new(pacode_types::UsageTotals::default()),
            permissions: crate::permissions::PermissionState::default(),
            questions: crate::questions::QuestionState::default(),
            events: crate::transcript::EventSink::new(),
            tasks: pacode_exec::TaskManager::new(
                temp.path().join("spool"),
                pacode_types::ExecConfig::default(),
            ),
            store: pacode_store::Store::open_in_memory().unwrap(),
            submit_lock: tokio::sync::Mutex::new(()),
            closed: std::sync::atomic::AtomicBool::new(false),
            recovery_admissions: std::sync::Mutex::new(VecDeque::new()),
            config: Arc::new(config),
            providers: Arc::new(pacode_provider::ProviderRegistry::empty()),
            tools: RwLock::new(tools),
            mcp: pacode_mcp::McpPool::new(Default::default(), None, None),
            plugins: Arc::new(pacode_plugin::PluginHost::new()),
            app_version: "test".to_string(),
            skills: Arc::new(pacode_skills::SkillRegistry::default()),
            scheduler: crate::schedule::Scheduler::new(),
            // The route starts flagged so `set_zdr(true)` passes the capable
            // check; tests that need an unflagged route clear it explicitly.
            model_flags: RwLock::new(HashMap::from([(
                "mock/mock-model".to_string(),
                ModelFlags {
                    zdr_capable: true,
                    ..ModelFlags::default()
                },
            )])),
            global_events: crate::transcript::EventSink::new(),
            idle_watch: crate::core::IdleWatch::default(),
        });
        (temp, session)
    }

    fn draft(tool_name: &str, kind: ToolKind) -> PermissionDraft {
        PermissionDraft {
            agent: AgentId::main(),
            agent_name: "main".to_string(),
            call_id: CallId::generate(),
            title: tool_name.to_string(),
            detail: String::new(),
            risk: None,
            tool_name: Some(tool_name.to_string()),
            tool_kind: Some(kind),
        }
    }

    #[tokio::test]
    async fn task_operations_reject_tasks_from_another_session() {
        let (_temp, session) = zdr_session(ZdrMode::Off);
        let other_session = pacode_types::SessionId::generate();
        let task = session
            .tasks
            .spawn(TaskSpec::new(
                other_session,
                AgentId::main(),
                "sleep 5",
                std::env::current_dir().unwrap(),
            ))
            .await
            .unwrap();
        let host = SessionHost {
            session: session.clone(),
            agent: AgentId::main(),
        };
        assert!(host.task_info(&task.id).is_none());
        assert!(host.kill_task(&task.id).await.is_err());
        session.tasks.kill(&task.id).await.unwrap();
    }

    #[test]
    fn edit_permission_cache_uses_stable_target_not_draft_detail() {
        assert_eq!(
            permission_target("write", "Write src/app.rs", "first diff"),
            "src/app.rs"
        );
        assert_eq!(
            permission_target("write", "Write src/app.rs", "second diff"),
            "src/app.rs"
        );
        assert_eq!(
            permission_target(
                "edit",
                "Edit src/app.rs (outside workspace)",
                "another diff"
            ),
            "src/app.rs"
        );
    }

    #[tokio::test]
    async fn zdr_active_modes() {
        let (_t, session) = zdr_session(ZdrMode::Session);
        assert!(!session.zdr_active(), "session mode + zdr off = inactive");
        session.set_zdr(true).unwrap();
        assert!(session.zdr_active(), "session mode + zdr on = active");

        let (_t, always) = zdr_session(ZdrMode::Always);
        assert!(
            always.zdr_active(),
            "always mode is active even with zdr off"
        );

        let (_t, off) = zdr_session(ZdrMode::Off);
        off.set_zdr(true).unwrap();
        assert!(!off.zdr_active(), "off mode ignores the meta flag");
    }

    #[tokio::test]
    async fn zdr_denies_network_and_mcp_tools() {
        let (_t, session) = zdr_session(ZdrMode::Session);
        session.set_zdr(true).unwrap();
        let host = SessionHost {
            session,
            agent: AgentId::main(),
        };

        let net = host
            .request_permission(draft("webfetch", ToolKind::Network))
            .await;
        assert_eq!(
            net.decision,
            PermissionDecision::Deny,
            "network tool must be denied"
        );
        assert!(
            net.reason
                .as_deref()
                .is_some_and(|r| r.contains("zdr_blocked")),
            "zdr deny must carry its reason, got {:?}",
            net.reason
        );

        let mcp = host
            .request_permission(draft("remote__query", ToolKind::Exec))
            .await;
        assert_eq!(
            mcp.decision,
            PermissionDecision::Deny,
            "mcp server__tool must be denied"
        );

        // Local read-only tool still passes through the gate -> AllowOnce.
        let fs = host
            .request_permission(draft("read", ToolKind::ReadOnly))
            .await;
        assert_eq!(
            fs.decision,
            PermissionDecision::AllowOnce,
            "fs tool must stay allowed under ZDR"
        );
    }

    #[tokio::test]
    async fn zdr_enable_rejects_unflagged_route() {
        let (_t, session) = zdr_session(ZdrMode::Session);
        session
            .set_model_flags("mock/mock-model".into(), ModelFlags::default())
            .unwrap();
        let err = session.set_zdr(true).unwrap_err().to_string();
        assert!(err.contains("zdr_blocked"), "unexpected error: {err}");
        assert!(
            !session.zdr_active(),
            "rejected enable must not flip the flag"
        );
    }

    #[tokio::test]
    async fn zdr_blocks_unmarking_active_route() {
        let (_t, session) = zdr_session(ZdrMode::Session);
        session.set_zdr(true).unwrap();
        let err = session
            .set_model_flags("mock/mock-model".into(), ModelFlags::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("zdr_blocked"), "unexpected error: {err}");
        // Favoriting the active route stays allowed — only unmarking fails.
        session
            .set_model_flags(
                "mock/mock-model".into(),
                ModelFlags {
                    zdr_capable: true,
                    favorite: true,
                },
            )
            .unwrap();
        // Other routes are unaffected by the guard.
        session
            .set_model_flags("mock/other".into(), ModelFlags::default())
            .unwrap();
    }

    #[tokio::test]
    async fn zdr_denies_subagent_spawn() {
        let (_t, session) = zdr_session(ZdrMode::Session);
        session.set_zdr(true).unwrap();
        let host = SessionHost {
            session,
            agent: AgentId::main(),
        };
        let res = host
            .spawn_agent(pacode_tools::AgentSpec {
                prompt: "hi".to_string(),
                name: None,
                model: None,
                effort: None,
                fork: false,
                tools: None,
                exclude_tools: None,
            })
            .await;
        match res {
            Err(pacode_tools::ToolError::Denied(msg)) => {
                assert!(msg.contains("zdr_blocked"), "deny reason: {msg}");
            }
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn zdr_off_leaves_permission_gate_alone() {
        let (_t, session) = zdr_session(ZdrMode::Session);
        let host = SessionHost {
            session,
            agent: AgentId::main(),
        };
        let net = host
            .request_permission(draft("webfetch", ToolKind::Network))
            .await;
        assert_eq!(net.decision, PermissionDecision::AllowOnce);
    }

    // ---- ZDR route policy: no provider call may leave the boundary, whether
    // it comes from the turn loop or from an auxiliary path the user never sees
    // in a permission prompt. ----

    #[tokio::test]
    async fn zdr_route_violation_follows_the_flag() {
        let (_t, session) = zdr_session(ZdrMode::Session);
        assert!(
            crate::permissions::zdr_route_violation(&session, "mock/mock-model").is_none(),
            "ZDR off: any route is allowed"
        );

        session.set_zdr(true).unwrap();
        assert!(
            crate::permissions::zdr_route_violation(&session, "mock/mock-model").is_none(),
            "flagged route stays usable under ZDR"
        );
        let reason = crate::permissions::zdr_route_violation(&session, "mock/other")
            .expect("an unflagged route must be refused under ZDR");
        assert!(
            reason.starts_with("zdr_blocked:"),
            "reason must be recognisable as policy: {reason}"
        );
    }

    #[tokio::test]
    async fn zdr_tool_violation_covers_network_and_mcp_only() {
        let (_t, session) = zdr_session(ZdrMode::Session);
        assert!(
            crate::permissions::zdr_tool_violation(&session, "webfetch", ToolKind::Network)
                .is_none()
        );
        session.set_zdr(true).unwrap();
        assert!(
            crate::permissions::zdr_tool_violation(&session, "webfetch", ToolKind::Network)
                .is_some()
        );
        assert!(
            crate::permissions::zdr_tool_violation(&session, "srv__tool", ToolKind::Exec).is_some()
        );
        assert!(
            crate::permissions::zdr_tool_violation(&session, "read", ToolKind::ReadOnly).is_none()
        );
    }

    // ---- The subagent route prompt is answered in the user's own time, so the
    // decision must be re-read against the state the spawn actually uses. ----

    fn agent_spec(model: Option<&str>) -> pacode_tools::AgentSpec {
        pacode_tools::AgentSpec {
            prompt: "hi".to_string(),
            name: None,
            model: model.map(session_model),
            effort: None,
            fork: false,
            tools: None,
            exclude_tools: None,
        }
    }

    fn session_model(model: &str) -> ModelRoute {
        ModelRoute {
            provider: "mock".into(),
            model: model.into(),
        }
    }

    /// The permission sits on the queue until a client answers; every caller
    /// here needs it there before it moves the session underneath it.
    async fn await_pending_permission(session: &Arc<Session>) -> pacode_types::PermissionId {
        for _ in 0..2_000 {
            if let Some(req) = session.permissions.pending().first() {
                return req.id.clone();
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        panic!("the request was never put up for a decision");
    }

    #[tokio::test]
    async fn zdr_enabled_while_the_route_prompt_is_open_refuses_the_spawn() {
        let (_t, session) = zdr_session_with_main_agent(ZdrMode::Session);
        let host = SessionHost {
            session: session.clone(),
            agent: AgentId::main(),
        };
        let spawn =
            tokio::spawn(async move { host.spawn_agent(agent_spec(Some("other-model"))).await });

        let perm_id = await_pending_permission(&session).await;
        session.set_zdr(true).unwrap();
        assert!(session.resolve_permission(&perm_id, PermissionDecision::AllowOnce, None));

        match spawn.await.unwrap() {
            Err(pacode_tools::ToolError::Denied(msg)) => {
                assert!(msg.contains("zdr_blocked"), "deny reason: {msg}");
            }
            other => panic!("expected a ZDR denial, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_route_switch_while_the_prompt_is_open_voids_the_decision() {
        let (_t, session) = zdr_session_with_main_agent(ZdrMode::Session);
        let host = SessionHost {
            session: session.clone(),
            agent: AgentId::main(),
        };
        let spawn =
            tokio::spawn(async move { host.spawn_agent(agent_spec(Some("other-model"))).await });

        let perm_id = await_pending_permission(&session).await;
        // The user moves the session elsewhere before answering the prompt.
        session
            .meta
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .model = session_model("moved-model");
        assert!(session.resolve_permission(&perm_id, PermissionDecision::AllowOnce, None));

        match spawn.await.unwrap() {
            Err(pacode_tools::ToolError::Denied(msg)) => {
                assert!(msg.contains("session route changed"), "deny reason: {msg}");
            }
            other => panic!("expected a stale-decision denial, got {other:?}"),
        }
    }

    // ---- The AllowSession lookup and the queue insert are one decision: a
    // granted target must not raise a second prompt. ----

    #[tokio::test]
    async fn allow_session_grant_is_reused_without_a_second_prompt() {
        let (_t, session) = zdr_session(ZdrMode::Session);
        let host = SessionHost {
            session: session.clone(),
            agent: AgentId::main(),
        };
        let asking = tokio::spawn({
            let host = SessionHost {
                session: session.clone(),
                agent: AgentId::main(),
            };
            async move { host.request_permission(draft("bash", ToolKind::Exec)).await }
        });

        let perm_id = await_pending_permission(&session).await;
        assert!(session.resolve_permission(&perm_id, PermissionDecision::AllowSession, None));
        let first = asking.await.unwrap();
        assert_eq!(first.decision, PermissionDecision::AllowSession);

        let second = host.request_permission(draft("bash", ToolKind::Exec)).await;
        assert_eq!(second.decision, PermissionDecision::AllowSession);
        assert!(
            session.permissions.pending().is_empty(),
            "a granted target must not raise another prompt"
        );
    }
}

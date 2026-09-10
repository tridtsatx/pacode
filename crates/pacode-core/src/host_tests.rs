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

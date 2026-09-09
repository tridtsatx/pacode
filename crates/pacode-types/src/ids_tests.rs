use super::*;

#[test]
fn session_id_generate_format_and_uniqueness() {
    let a = SessionId::generate();
    let b = SessionId::generate();

    assert_ne!(a, b);

    let s = a.as_str();
    assert!(s.starts_with("pacode-"), "expected pacode- prefix, got {s}");
    assert_eq!(
        s.len(),
        20,
        "expected pacode-xxxxxxxx-xxxx length 20, got {s}"
    );

    let parts: Vec<&str> = s["pacode-".len()..].split('-').collect();
    assert_eq!(parts.len(), 2, "expected 2 parts after prefix, got {s}");
    assert_eq!(parts[0].len(), 8, "expected 8 hex chars, got {}", parts[0]);
    assert_eq!(parts[1].len(), 4, "expected 4 hex chars, got {}", parts[1]);

    assert!(
        parts[0]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "expected lowercase hex for part 0: {}",
        parts[0]
    );
    assert!(
        parts[1]
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "expected lowercase hex for part 1: {}",
        parts[1]
    );
}

#[test]
fn other_generated_ids_have_prefix_and_are_unique() {
    let a = AgentId::generate();
    let b = AgentId::generate();
    assert!(a.as_str().starts_with("agt_"));
    assert_ne!(a, b);

    let t = TurnId::generate();
    assert!(t.as_str().starts_with("trn_"));

    let c = CallId::generate();
    assert!(c.as_str().starts_with("call_"));

    let task = TaskId::generate();
    assert!(task.as_str().starts_with("tsk_"));

    let p = PermissionId::generate();
    assert!(p.as_str().starts_with("perm_"));

    let cl = ClientId::generate();
    assert!(cl.as_str().starts_with("cli_"));
}

#[test]
fn main_agent() {
    assert!(AgentId::main().is_main());
    assert!(!AgentId::generate().is_main());
    let json = serde_json::to_string(&AgentId::main()).expect("serialize main agent");
    assert_eq!(json, "\"main\"");
}

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
fn generated_ids_keep_the_prefix_and_twelve_hex_shape() {
    let id = TaskId::generate();
    let s = id.as_str();
    assert_eq!(s.len(), "tsk_".len() + 12, "{s}");
    let payload = &s["tsk_".len()..];
    assert!(
        payload
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "expected lowercase hex payload: {payload}"
    );
    assert_eq!(TaskId::PREFIX, "tsk");
}

#[test]
fn generated_ids_split_into_a_stable_nonce_and_a_moving_counter() {
    let first = payload(AgentId::generate());
    let second = payload(AgentId::generate());
    let (nonce_a, counter_a) = split(&first);
    let (nonce_b, counter_b) = split(&second);
    assert_eq!(nonce_b, nonce_a, "the nonce is fixed for the process");
    assert!(counter_b > counter_a, "the counter only moves forward");
}

#[test]
fn a_burst_of_generated_ids_never_repeats() {
    let mut seen = std::collections::HashSet::with_capacity(20_000);
    for _ in 0..20_000 {
        let id = TaskId::generate();
        assert!(seen.insert(id.as_str().to_string()), "repeated id {id}");
    }
}

#[test]
fn every_id_issued_after_another_one_is_a_larger_payload() {
    let mut previous = value(CallId::generate());
    for _ in 0..1_000 {
        let current = value(CallId::generate());
        assert!(
            current > previous,
            "payload {current:012x} did not follow {previous:012x}"
        );
        previous = current;
    }
}

#[test]
fn ids_stay_unique_when_threads_generate_them_at_once() {
    let threads: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(|| {
                (0..2_000)
                    .map(|_| TurnId::generate().as_str().to_string())
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut seen = std::collections::HashSet::with_capacity(16_000);
    for thread in threads {
        for id in thread.join().expect("generator thread") {
            assert!(seen.insert(id.clone()), "repeated id {id}");
        }
    }
    assert_eq!(seen.len(), 16_000);
}

fn payload<T: AsRef<str>>(id: T) -> String {
    let s = id.as_ref();
    s[s.find('_').expect("generated ids carry a prefix") + 1..].to_string()
}

fn value<T: AsRef<str>>(id: T) -> u64 {
    u64::from_str_radix(&payload(id), 16).expect("12 hex digits")
}

fn split(payload: &str) -> (u32, u64) {
    assert_eq!(payload.len(), 12, "{payload}");
    (
        u32::from_str_radix(&payload[..4], 16).expect("nonce hex"),
        u64::from_str_radix(&payload[4..], 16).expect("counter hex"),
    )
}

#[test]
fn main_agent() {
    assert!(AgentId::main().is_main());
    assert!(!AgentId::generate().is_main());
    let json = serde_json::to_string(&AgentId::main()).expect("serialize main agent");
    assert_eq!(json, "\"main\"");
}

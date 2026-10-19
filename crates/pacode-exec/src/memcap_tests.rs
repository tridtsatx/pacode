use pacode_types::UiConfig;

use super::*;

const GIB: u64 = 1024 * 1024 * 1024;
/// A round machine, so every expected number is readable as a fraction of it.
const MACHINE: u64 = 100 * GIB;

fn policy(percent: u8, split: bool) -> MemoryPolicy {
    MemoryPolicy {
        percent,
        split_sessions: split,
        machine_ram_bytes: MACHINE,
    }
}

#[test]
fn ceiling_divides_by_the_number_of_live_sessions() {
    let policy = policy(80, true);
    // Zero live sessions is the instant before the first session opens: it must
    // not divide by zero, and the session about to start takes the whole thing.
    assert_eq!(policy.ceiling_for(0), Some(80 * GIB));
    assert_eq!(policy.ceiling_for(1), Some(80 * GIB));
    assert_eq!(policy.ceiling_for(2), Some(40 * GIB));
    assert_eq!(policy.ceiling_for(4), Some(20 * GIB));
}

#[test]
fn the_shares_add_up_to_the_ceiling_and_never_past_it() {
    let policy = policy(80, true);
    for sessions in 1usize..=8 {
        let share = policy.ceiling_for(sessions).unwrap();
        let total = share * sessions as u64;
        assert!(
            total <= 80 * GIB,
            "{sessions} sessions may allocate {total}, over the {}-byte ceiling",
            80 * GIB
        );
        // Rounding each share down to a page costs at most a page per session.
        // Rounding up instead would hand the machine more than was configured,
        // which is the whole reason a ceiling is a ceiling.
        assert!(
            80 * GIB - total < sessions as u64 * PAGE_BYTES,
            "{sessions} sessions lose more than a page each to rounding"
        );
    }
}

#[test]
fn without_the_split_every_session_gets_the_whole_ceiling() {
    let policy = policy(80, false);
    assert_eq!(policy.ceiling_for(1), Some(80 * GIB));
    assert_eq!(policy.ceiling_for(4), Some(80 * GIB));
}

#[test]
fn zero_percent_disables_the_cap_entirely() {
    let policy = policy(0, true);
    assert!(!policy.is_enabled());
    assert_eq!(policy.ceiling_for(0), None);
    assert_eq!(policy.ceiling_for(1), None);
    assert_eq!(policy.ceiling_for(64), None);
    assert_eq!(policy.hard_max_bytes(), None);
}

#[test]
fn an_unknown_machine_size_disables_rather_than_guesses() {
    let policy = MemoryPolicy {
        percent: 80,
        split_sessions: true,
        machine_ram_bytes: 0,
    };
    assert!(!policy.is_enabled());
    assert_eq!(policy.ceiling_for(1), None);
}

#[test]
fn division_stops_at_the_floor_instead_of_shrinking_to_nothing() {
    let policy = policy(80, true);
    // 100 live sessions would be 0.8% of RAM each, which is not a safety
    // limit, it is an outage. The floor is TOOL_MEMORY_MIN_PERCENT.
    let hundred = policy.ceiling_for(100).unwrap();
    assert_eq!(hundred, 5 * GIB);
    assert_eq!(policy.division_floor(), 5 * GIB);
    // The floor is a floor, not a new ceiling: below it the shares keep
    // shrinking, and a single session still gets the full 80%.
    assert!(policy.ceiling_for(1000).unwrap() <= hundred);
    assert_eq!(policy.ceiling_for(1), Some(80 * GIB));
}

#[test]
fn the_ceiling_is_rounded_down_to_a_whole_page() {
    // 8 GiB + 1 byte of machine, so the percentage lands mid-page.
    let policy = MemoryPolicy {
        percent: 80,
        split_sessions: true,
        machine_ram_bytes: 8 * GIB + 1,
    };
    let ceiling = policy.ceiling_for(1).unwrap();
    assert_eq!(ceiling % PAGE_BYTES, 0);
    // Rounding up would hand out a page the user never configured.
    assert!(ceiling <= policy.machine_ram_bytes * 80 / 100);
}

#[test]
fn the_hard_limit_is_the_whole_machine() {
    let policy = policy(80, true);
    assert_eq!(policy.hard_max_bytes(), Some(MACHINE));
    // A live process's soft limit may be raised all the way to that, which is
    // what makes the raise a write rather than a re-exec.
    assert!(policy.hard_max_bytes().unwrap() > policy.ceiling_for(1).unwrap());
}

#[test]
fn the_ceiling_reports_what_enforces_it_and_what_it_was_divided_by() {
    let ceiling = MemoryCeiling {
        bytes: 40 * GIB,
        hard_max_bytes: MACHINE,
        kind: LimitKind::Cgroup,
        live_sessions: 2,
        percent: 80,
        split_sessions: true,
        machine_ram_bytes: MACHINE,
    };
    let described = ceiling.describe();
    assert!(described.contains("40.0 GiB"), "{described}");
    assert!(described.contains("2 live sessions"), "{described}");
    assert!(described.contains("cgroup-v2"), "{described}");

    let env = ceiling.environment();
    let lookup = |key: &str| {
        env.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(
        lookup("PACODE_MEMORY_LIMIT_BYTES").as_deref(),
        Some("42949672960")
    );
    assert_eq!(
        lookup("PACODE_MEMORY_HARD_MAX_BYTES").as_deref(),
        Some("107374182400")
    );
    assert_eq!(
        lookup("PACODE_MEMORY_LIMIT_KIND").as_deref(),
        Some("cgroup-v2")
    );
    // The address-space backend has to announce itself, or a script cannot
    // tell a memory ceiling from an address-space one.
    let mut va = ceiling.clone();
    va.kind = LimitKind::AddressSpace;
    assert!(
        va.environment().iter().any(|(name, value)| {
            name == "PACODE_MEMORY_LIMIT_KIND" && value == "address-space"
        })
    );
}

#[test]
fn config_percent_and_split_flow_into_the_policy() {
    let mut ui = UiConfig::default();
    assert_eq!(ui.tool_memory_percent, 80);
    assert!(ui.tool_memory_split_sessions);
    let policy = MemoryPolicy::from_ui(&ui, MACHINE);
    assert_eq!(policy.percent, 80);
    assert!(policy.split_sessions);
    assert_eq!(policy.ceiling_for(4), Some(20 * GIB));

    ui.tool_memory_percent = 0;
    assert!(!MemoryPolicy::from_ui(&ui, MACHINE).is_enabled());
}

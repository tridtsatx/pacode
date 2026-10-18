use std::time::{Duration, Instant};

use pacode_types::{ResourcesConfig, TaskId};

use super::*;

// 10_000 MB total: reserve 2000 MB, breach level 1488 MB (overshoot 512 MB).
const TOTAL_MB: u64 = 10_000;

fn params() -> PolicyParams {
    PolicyParams::from(&ResourcesConfig::default())
}

fn host(avail_mb: u64) -> HostSample {
    HostSample {
        total: TOTAL_MB * MB,
        available: avail_mb * MB,
        swap_used: 0,
        disk_free: None,
    }
}

fn tree(name: &str, rss_mb: u64, started: Instant) -> TreeSample {
    TreeSample {
        id: TaskId::new(name),
        rss: rss_mb * MB,
        started,
        protected: false,
    }
}

fn secs(t0: Instant, s: u64) -> Instant {
    t0 + Duration::from_secs(s)
}

fn kills(ds: &[Decision]) -> Vec<(String, KillSignal)> {
    ds.iter()
        .filter_map(|d| match d {
            Decision::Kill { tree, signal, .. } => Some((tree.as_str().to_string(), *signal)),
            Decision::Warn { .. } | Decision::Refuse { .. } | Decision::Clear => None,
        })
        .collect()
}

#[test]
fn warn_zone_refuses_but_never_kills() {
    let t0 = Instant::now();
    let mut p = Policy::new(params());
    let trees = [tree("a", 5000, t0)];
    let first = p.step(secs(t0, 3), host(1800), &trees);
    assert!(first.iter().any(|d| matches!(
        d,
        Decision::Warn {
            level: Level::Warn,
            ..
        }
    )));
    let msg = first.iter().find_map(|d| match d {
        Decision::Refuse {
            kind: RefuseKind::Memory,
            message,
        } => Some(message.clone()),
        _ => None,
    });
    let msg = msg.unwrap_or_default();
    assert!(msg.starts_with("low resources: "), "{msg}");
    assert!(msg.contains("reserve is 20%"), "{msg}");
    assert!(msg.ends_with("(see /bg)"), "{msg}");
    for s in 4..120 {
        let ds = p.step(secs(t0, s), host(1800), &trees);
        assert!(kills(&ds).is_empty());
    }
    assert_eq!(p.level(), Level::Warn);
}

#[test]
fn short_overshoot_blip_never_kills() {
    let t0 = Instant::now();
    let mut p = Policy::new(params());
    let trees = [tree("a", 5000, t0)];
    let mut cleared = 0;
    for s in 3..40 {
        let avail = if (3..6).contains(&s) { 1000 } else { 2500 };
        let ds = p.step(secs(t0, s), host(avail), &trees);
        assert!(kills(&ds).is_empty(), "killed at {s}");
        cleared += ds.iter().filter(|d| **d == Decision::Clear).count();
    }
    assert_eq!(cleared, 1);
    assert_eq!(p.level(), Level::Ok);
}

#[test]
fn sustained_breach_kills_largest_with_term_then_kill() {
    let t0 = Instant::now();
    let mut p = Policy::new(params());
    let trees = [tree("small", 500, t0), tree("big", 3000, t0)];
    for s in 3..13 {
        assert!(
            kills(&p.step(secs(t0, s), host(1000), &trees)).is_empty(),
            "early kill at {s}"
        );
    }
    let ds = p.step(secs(t0, 13), host(1000), &trees);
    assert_eq!(kills(&ds), vec![("big".to_string(), KillSignal::Term)]);
    let reason = ds.iter().find_map(|d| match d {
        Decision::Kill { reason, .. } => Some(reason.clone()),
        _ => None,
    });
    let reason = reason.unwrap_or_default();
    assert!(
        reason.starts_with("killed by pacode resource guard: process tree used 2.9 GB; host had 1000 MB (10%) free, reserve is 20%."),
        "{reason}"
    );
    for s in 14..18 {
        assert!(
            kills(&p.step(secs(t0, s), host(1000), &trees)).is_empty(),
            "at {s}"
        );
    }
    let ds = p.step(secs(t0, 18), host(1000), &trees);
    assert_eq!(kills(&ds), vec![("big".to_string(), KillSignal::Kill)]);
}

#[test]
fn protected_and_young_trees_are_skipped() {
    let t0 = Instant::now();
    let mut p = Policy::new(params());
    let mut huge = tree("huge", 6000, t0);
    huge.protected = true;
    let young = tree("young", 4000, secs(t0, 12));
    let ok = tree("ok", 300, t0);
    let trees = [huge, young, ok];
    for s in 3..13 {
        p.step(secs(t0, s), host(1000), &trees);
    }
    let ds = p.step(secs(t0, 13), host(1000), &trees);
    assert_eq!(kills(&ds), vec![("ok".to_string(), KillSignal::Term)]);
}

#[test]
fn only_protected_trees_means_no_kill() {
    let t0 = Instant::now();
    let mut p = Policy::new(params());
    let mut t = tree("only", 6000, t0);
    t.protected = true;
    let trees = [t];
    for s in 3..40 {
        assert!(kills(&p.step(secs(t0, s), host(1000), &trees)).is_empty());
    }
}

#[test]
fn hysteresis_prevents_flapping() {
    let t0 = Instant::now();
    let mut p = Policy::new(params());
    let trees = [tree("a", 100, t0)];
    let mut clears = Vec::new();
    let mut warns = 0;
    // breach, then oscillation around the reserve, then a steady recovery
    let series = [
        (3, 1000),
        (4, 2100),
        (5, 1900),
        (6, 2100),
        (7, 2100),
        (8, 2100),
    ];
    for (s, avail) in series {
        let ds = p.step(secs(t0, s), host(avail), &trees);
        warns += ds
            .iter()
            .filter(|d| matches!(d, Decision::Warn { .. }))
            .count();
        if ds.contains(&Decision::Clear) {
            clears.push(s);
        }
    }
    // Cleared once, 2 s after the first sample that stayed above the reserve (t=6).
    assert_eq!(clears, vec![8]);
    assert_eq!(
        warns, 1,
        "no re-emitted warnings while the level did not change"
    );
}

#[test]
fn cooldown_between_kills() {
    let t0 = Instant::now();
    let mut p = Policy::new(params());
    let trees = [tree("a", 3000, t0), tree("b", 2500, t0)];
    let mut term_at = Vec::new();
    for s in 3..40 {
        for (id, sig) in kills(&p.step(secs(t0, s), host(1000), &trees)) {
            if sig == KillSignal::Term {
                term_at.push((s, id));
            }
        }
    }
    assert_eq!(term_at, vec![(13, "a".to_string()), (23, "b".to_string())]);
}

#[test]
fn disabled_policy_is_a_no_op() {
    let t0 = Instant::now();
    let cfg = ResourcesConfig {
        enabled: false,
        ..ResourcesConfig::default()
    };
    let mut p = Policy::new(PolicyParams::from(&cfg));
    let trees = [tree("a", 9000, t0)];
    for s in 3..40 {
        assert!(p.step(secs(t0, s), host(10), &trees).is_empty());
    }
    assert_eq!(refusal(&PolicyParams::from(&cfg), &host(10)), None);
}

#[test]
fn per_process_cap_uses_grace_and_overshoot() {
    let t0 = Instant::now();
    let cfg = ResourcesConfig {
        max_process_percent: 30,
        ..ResourcesConfig::default()
    };
    let mut p = Policy::new(PolicyParams::from(&cfg));
    // cap 3000 MB + 512 MB overshoot: 3400 MB is tolerated forever.
    let fine = [tree("a", 3400, t0)];
    for s in 3..40 {
        assert!(kills(&p.step(secs(t0, s), host(8000), &fine)).is_empty());
    }
    let over = [tree("a", 4000, t0)];
    for s in 40..50 {
        assert!(kills(&p.step(secs(t0, s), host(8000), &over)).is_empty());
    }
    let ds = p.step(secs(t0, 50), host(8000), &over);
    assert_eq!(kills(&ds), vec![("a".to_string(), KillSignal::Term)]);
    let text = format!("{ds:?}");
    assert!(text.contains("per-process limit"), "{text}");
}

#[test]
fn sampling_interval_adapts_to_headroom() {
    let p = Policy::new(params());
    // headroom threshold: 2000 + 2*512 = 3024 MB
    assert_eq!(p.sample_interval(&host(5000)), Duration::from_millis(1000));
    assert_eq!(p.sample_interval(&host(2500)), Duration::from_millis(200));
}

#[test]
fn low_disk_refuses_with_disk_kind() {
    let t0 = Instant::now();
    let mut p = Policy::new(params());
    let mut h = host(8000);
    h.disk_free = Some(100 * MB);
    let ds = p.step(secs(t0, 1), h, &[]);
    assert!(ds.iter().any(|d| matches!(
        d,
        Decision::Refuse {
            kind: RefuseKind::Disk,
            ..
        }
    )));
    assert!(refusal(&params(), &h).is_some());
}

#[test]
fn refusal_text_is_stable() {
    let msg = refusal(&params(), &host(1000)).unwrap_or_default();
    assert_eq!(
        msg,
        "low resources: 1000 MB free of 9.8 GB (10%), reserve is 20%; wait for running commands to finish or stop one (see /bg)"
    );
    assert_eq!(refusal(&params(), &host(2500)), None);
}

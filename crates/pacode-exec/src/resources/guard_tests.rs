use std::sync::Mutex;
use std::time::{Duration, Instant};

use pacode_types::ResourcesConfig;

use super::*;

struct FakeSampler {
    host: Mutex<HostSample>,
    supported: bool,
}

impl FakeSampler {
    fn new(available_mb: u64, supported: bool) -> Arc<Self> {
        Arc::new(Self {
            host: Mutex::new(HostSample {
                total: 10_000 << 20,
                available: available_mb << 20,
                swap_used: 0,
                disk_free: None,
            }),
            supported,
        })
    }
}

impl ResourceSampler for FakeSampler {
    fn supported(&self) -> bool {
        self.supported
    }
    fn host(&self, _spool_dir: &std::path::Path) -> Option<HostSample> {
        Some(*self.host.lock().unwrap_or_else(|e| e.into_inner()))
    }
    fn trees(&self, roots: &[TreeRoot]) -> Vec<super::super::sampler::TreeSample> {
        roots
            .iter()
            .map(|r| super::super::sampler::TreeSample {
                id: r.id.clone(),
                rss: 1 << 20,
                started: r.started,
                protected: r.protected,
            })
            .collect()
    }
}

#[derive(Default)]
struct FakeHost {
    kills: Mutex<Vec<(TaskId, KillSignal)>>,
}

impl GuardHost for FakeHost {
    fn roots(&self) -> Vec<TreeRoot> {
        vec![TreeRoot {
            id: TaskId::new("t1"),
            pgid: 1,
            started: Instant::now(),
            protected: false,
        }]
    }
    fn apply_kill(&self, id: &TaskId, signal: KillSignal, _reason: &str) {
        self.kills
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((id.clone(), signal));
    }
    fn notice(&self, _notice: ResourceNotice) {}
}

fn guard(sampler: Arc<FakeSampler>, cfg: &ResourcesConfig) -> ResourceGuard {
    ResourceGuard::with_sampler(cfg, std::env::temp_dir(), sampler)
}

async fn wait_until(mut f: impl FnMut() -> bool) -> bool {
    for _ in 0..100 {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f()
}

#[test]
fn idle_guard_has_no_sampling_task() {
    let g = guard(FakeSampler::new(8000, true), &ResourcesConfig::default());
    assert!(!g.is_sampling());
    assert!(g.spool_allowed());
}

#[test]
fn admit_refuses_below_reserve_and_passes_above() {
    let cfg = ResourcesConfig::default();
    let low = guard(FakeSampler::new(1500, true), &cfg);
    match low.admit() {
        Err(ExecError::LowResources(m)) => assert!(m.starts_with("low resources: "), "{m}"),
        other => panic!("expected LowResources, got {other:?}"),
    }
    assert!(matches!(low.admit_agent(), Err(ExecError::LowResources(_))));
    let fine = guard(FakeSampler::new(5000, true), &cfg);
    assert!(fine.admit().is_ok());
}

#[test]
fn disabled_guard_admits_everything() {
    let cfg = ResourcesConfig {
        enabled: false,
        ..ResourcesConfig::default()
    };
    let g = guard(FakeSampler::new(1, true), &cfg);
    assert!(g.admit().is_ok());
    assert!(!g.enabled());
}

#[tokio::test]
async fn sampling_lives_exactly_as_long_as_a_process() {
    let g = guard(FakeSampler::new(8000, true), &ResourcesConfig::default());
    let host: Arc<dyn GuardHost> = Arc::new(FakeHost::default());
    g.bind(Arc::downgrade(&host));
    g.process_started();
    assert!(g.is_sampling());
    g.process_started();
    g.process_ended();
    assert!(g.is_sampling(), "one process still alive");
    g.process_ended();
    assert!(
        wait_until(|| !g.is_sampling()).await,
        "task must end with the last process"
    );
}

#[tokio::test]
async fn unsupported_platform_never_starts_sampling() {
    let g = guard(FakeSampler::new(8000, false), &ResourcesConfig::default());
    g.process_started();
    assert!(!g.is_sampling());
    assert!(g.admit().is_ok());
}

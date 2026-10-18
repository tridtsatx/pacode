//! `ResourceGuard`: the lifecycle around the pure [`Policy`].
//!
//! - Sampling runs in ONE tokio task that exists only while at least one supervised
//!   process is alive: `process_started` spawns it on the 0 -> 1 transition and it
//!   exits (woken immediately through a `Notify`, no idle timer) once the count is 0
//!   again. With no process there is no task, no timer and no sampling.
//! - Each tick reads the host and the supervised trees on a blocking thread, feeds
//!   the policy and applies its decisions through [`GuardHost`] (the task manager).
//! - `admit` / `admit_agent` take a fresh host sample on demand (a single small file
//!   read) instead of trusting stale state; they never tick.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::Instant;

use pacode_types::{ResourcesConfig, TaskId};
use tokio::sync::Notify;

use super::policy::{
    Decision, KillSignal, Level, Policy, PolicyParams, RefuseKind, free_percent, memory_message,
    refusal,
};
use super::sampler::{HostSample, ResourceSampler, TreeRoot};
use crate::manager::ExecError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    Warn,
    Breach,
    Clear,
}

/// What the manager forwards to clients (as `TaskEvent::Resource`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceNotice {
    pub level: NoticeLevel,
    pub free_percent: u64,
    pub message: String,
}

/// The side effects the guard needs from its owner (implemented by the task manager).
pub trait GuardHost: Send + Sync {
    /// Running supervised trees right now.
    fn roots(&self) -> Vec<TreeRoot>;
    /// Signal the process group of `id`; `reason` is recorded for the task on SIGTERM.
    fn apply_kill(&self, id: &TaskId, signal: KillSignal, reason: &str);
    fn notice(&self, notice: ResourceNotice);
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct Life {
    live: usize,
    running: bool,
}

struct Inner {
    params: PolicyParams,
    spool_dir: PathBuf,
    sampler: Arc<dyn ResourceSampler>,
    host: OnceLock<Weak<dyn GuardHost>>,
    life: Mutex<Life>,
    wake: Notify,
    spool_blocked: AtomicBool,
    unsupported_logged: AtomicBool,
}

#[derive(Clone)]
pub struct ResourceGuard {
    inner: Arc<Inner>,
}

impl ResourceGuard {
    pub fn new(cfg: &ResourcesConfig, spool_dir: PathBuf) -> Self {
        Self::with_sampler(cfg, spool_dir, super::default_sampler())
    }

    pub fn with_sampler(
        cfg: &ResourcesConfig,
        spool_dir: PathBuf,
        sampler: Arc<dyn ResourceSampler>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                params: PolicyParams::from(cfg),
                spool_dir,
                sampler,
                host: OnceLock::new(),
                life: Mutex::new(Life {
                    live: 0,
                    running: false,
                }),
                wake: Notify::new(),
                spool_blocked: AtomicBool::new(false),
                unsupported_logged: AtomicBool::new(false),
            }),
        }
    }

    /// Connect the owner. Held weakly so the guard never keeps its owner alive.
    pub fn bind(&self, host: Weak<dyn GuardHost>) {
        let _ = self.inner.host.set(host);
    }

    pub fn enabled(&self) -> bool {
        self.inner.params.enabled
    }

    /// True while a sampling task exists (diagnostics and tests).
    pub fn is_sampling(&self) -> bool {
        lock(&self.inner.life).running
    }

    /// Refuse a new process when resources are low.
    pub fn admit(&self) -> Result<(), ExecError> {
        let inner = &self.inner;
        if !inner.params.enabled || !inner.sampler.supported() {
            return Ok(());
        }
        let Some(host) = inner.sampler.host(&inner.spool_dir) else {
            return Ok(());
        };
        match refusal(&inner.params, &host) {
            Some(msg) => Err(ExecError::LowResources(msg)),
            None => Ok(()),
        }
    }

    /// Same check for a new subagent (it is about to start commands and models).
    pub fn admit_agent(&self) -> Result<(), ExecError> {
        self.admit()
    }

    /// False once free disk on the spool directory is below the minimum: readers stop
    /// appending to the spool and write a final marker.
    pub fn spool_allowed(&self) -> bool {
        !self.inner.spool_blocked.load(Ordering::Relaxed)
    }

    /// A supervised process started. Starts the sampling task on the first one.
    pub fn process_started(&self) {
        let inner = &self.inner;
        if !inner.params.enabled {
            return;
        }
        if !inner.sampler.supported() {
            if !inner.unsupported_logged.swap(true, Ordering::Relaxed) {
                log::warn!(
                    "resource guard disabled: memory sampling is unsupported on this platform"
                );
            }
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let mut life = lock(&inner.life);
        life.live += 1;
        if !life.running {
            life.running = true;
            handle.spawn(run(Arc::clone(inner)));
        }
    }

    /// A supervised process ended. Wakes the task so it can exit at once on zero.
    pub fn process_ended(&self) {
        let inner = &self.inner;
        if !inner.params.enabled || !inner.sampler.supported() {
            return;
        }
        let mut life = lock(&inner.life);
        life.live = life.live.saturating_sub(1);
        if life.live == 0 {
            inner.wake.notify_one();
        }
    }
}

fn stop(inner: &Inner, policy: &Policy, host: Option<&Arc<dyn GuardHost>>) {
    inner.spool_blocked.store(false, Ordering::Relaxed);
    if policy.level() != Level::Ok
        && let Some(h) = host
    {
        h.notice(ResourceNotice {
            level: NoticeLevel::Clear,
            free_percent: 100,
            message: String::new(),
        });
    }
}

async fn run(inner: Arc<Inner>) {
    let mut policy = Policy::new(inner.params.clone());
    let mut interval = inner.params.idle_interval;
    loop {
        {
            let mut life = lock(&inner.life);
            if life.live == 0 {
                life.running = false;
                drop(life);
                let host = inner.host.get().and_then(Weak::upgrade);
                stop(&inner, &policy, host.as_ref());
                return;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(interval) => {}
            _ = inner.wake.notified() => {}
        }
        if lock(&inner.life).live == 0 {
            continue;
        }
        let Some(host) = inner.host.get().and_then(Weak::upgrade) else {
            lock(&inner.life).running = false;
            stop(&inner, &policy, None);
            return;
        };
        let roots = host.roots();
        if roots.is_empty() {
            continue;
        }
        let sampler = Arc::clone(&inner.sampler);
        let dir = inner.spool_dir.clone();
        let sampled = tokio::task::spawn_blocking(move || {
            let h = sampler.host(&dir)?;
            Some((h, sampler.trees(&roots)))
        })
        .await;
        let Ok(Some((hs, trees))) = sampled else {
            continue;
        };
        let decisions = policy.step(Instant::now(), hs, &trees);
        interval = policy.sample_interval(&hs);
        apply(&inner, &*host, &decisions, &hs);
    }
}

fn apply(inner: &Inner, host: &dyn GuardHost, decisions: &[Decision], hs: &HostSample) {
    let mut disk_blocked = false;
    for d in decisions {
        match d {
            Decision::Warn { level, .. } => host.notice(ResourceNotice {
                level: match level {
                    Level::Breach => NoticeLevel::Breach,
                    Level::Warn | Level::Ok => NoticeLevel::Warn,
                },
                free_percent: free_percent(hs),
                message: memory_message(&inner.params, hs),
            }),
            Decision::Refuse { kind, .. } => {
                if *kind == RefuseKind::Disk {
                    disk_blocked = true;
                }
            }
            Decision::Kill {
                tree,
                signal,
                reason,
            } => host.apply_kill(tree, *signal, reason),
            Decision::Clear => host.notice(ResourceNotice {
                level: NoticeLevel::Clear,
                free_percent: free_percent(hs),
                message: String::new(),
            }),
        }
    }
    inner.spool_blocked.store(disk_blocked, Ordering::Relaxed);
}

#[cfg(test)]
#[path = "guard_tests.rs"]
mod guard_tests;

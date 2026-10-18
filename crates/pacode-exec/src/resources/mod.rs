//! Machine-resource guard: keeps a share of memory (and spool disk) free by refusing
//! new processes when resources are low and softly stopping the largest supervised
//! process tree on a sustained breach. See `policy.rs` for the rules.

pub mod fallback;
pub mod guard;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod policy;
pub mod sampler;

use std::sync::Arc;

pub use guard::{GuardHost, NoticeLevel, ResourceGuard, ResourceNotice};
pub use policy::{Decision, KillSignal, Level, Policy, PolicyParams, RefuseKind};
pub use sampler::{HostSample, ResourceSampler, TreeRoot, TreeSample};

/// Sampler for the current platform.
#[cfg(target_os = "linux")]
pub fn default_sampler() -> Arc<dyn ResourceSampler> {
    Arc::new(linux::LinuxSampler::new())
}

#[cfg(target_os = "macos")]
pub fn default_sampler() -> Arc<dyn ResourceSampler> {
    Arc::new(fallback::MacSampler)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn default_sampler() -> Arc<dyn ResourceSampler> {
    Arc::new(fallback::UnsupportedSampler)
}

//! Reasoning-effort clamping: an effort the model does not support never reaches
//! the session, whichever client asked for it (TUI, ACP, subagent inheritance).
//!
//! The lookup never waits on the network: a cached catalog answer or the
//! provider's static `model_info` is returned at once, and a background task
//! fetches the catalog and fills the cache for the next call.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use pacode_provider::Provider;
use pacode_types::{Effort, ModelRoute};

use crate::session::Session;

/// Most routes cached per session; the map is cleared when a new one overflows.
const CACHE_CAP: usize = 32;

#[derive(Default)]
struct Inner {
    known: HashMap<ModelRoute, Vec<Effort>>,
    /// Routes with a background fetch in flight (at most one per route).
    fetching: HashSet<ModelRoute>,
}

/// Catalog-derived supported efforts per `ModelRoute`.
///
/// Cap: 32 routes, cleared on overflow. Owner: the `Session` (cloned `Arc`s
/// live only in short background fetch tasks). Invalidation: `Session::set_model`
/// to a different route drops that route's entry so the catalog is re-read.
#[derive(Clone, Default)]
pub struct EffortsCache(Arc<Mutex<Inner>>);

impl EffortsCache {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn get(&self, route: &ModelRoute) -> Option<Vec<Effort>> {
        self.lock().known.get(route).cloned()
    }

    fn put(&self, route: &ModelRoute, efforts: Vec<Effort>) {
        let mut inner = self.lock();
        if inner.known.len() >= CACHE_CAP && !inner.known.contains_key(route) {
            inner.known.clear();
        }
        inner.known.insert(route.clone(), efforts);
    }

    /// Claim the fetch slot for `route`; false when one is already running.
    fn begin_fetch(&self, route: &ModelRoute) -> bool {
        self.lock().fetching.insert(route.clone())
    }

    fn end_fetch(&self, route: &ModelRoute) {
        self.lock().fetching.remove(route);
    }

    /// Forget `route`'s cached answer.
    pub(crate) fn invalidate(&self, route: &ModelRoute) {
        self.lock().known.remove(route);
    }
}

/// Read the provider catalog (no timeout) and cache the efforts of `route`.
async fn fetch_into(cache: &EffortsCache, provider: &dyn Provider, route: &ModelRoute) {
    let listed = provider
        .list_models()
        .await
        .ok()
        .and_then(|all| all.into_iter().find(|m| m.route.model == route.model));
    if let Some(info) = listed {
        cache.put(route, info.efforts);
    }
}

impl Session {
    /// Reasoning levels `route` supports; empty when the model has no reasoning
    /// control or nothing is known about it. Never waits on the network.
    pub(crate) async fn supported_efforts(&self, route: &ModelRoute) -> Vec<Effort> {
        if let Some(hit) = self.efforts_cache.get(route) {
            return hit;
        }
        let Ok(provider) = self.providers.resolve(route).await else {
            return Vec::new();
        };
        // One background fetch per route; `begin_fetch` is false while one runs.
        if self.efforts_cache.begin_fetch(route) {
            let cache = self.efforts_cache.clone();
            let task_provider = provider.clone();
            let task_route = route.clone();
            tokio::spawn(async move {
                fetch_into(&cache, task_provider.as_ref(), &task_route).await;
                cache.end_fetch(&task_route);
            });
        }
        provider.model_info(&route.model).efforts
    }

    /// `effort` moved to the nearest level `route` supports (preferring the next
    /// lower one). Unchanged when the model's levels are unknown or it has none.
    pub(crate) async fn clamp_effort_for(&self, route: &ModelRoute, effort: Effort) -> Effort {
        let supported = self.supported_efforts(route).await;
        effort.clamp_to(&supported).unwrap_or(effort)
    }

    /// Drop the cached efforts of `route` (called when the session switches to it).
    pub(crate) fn invalidate_efforts(&self, route: &ModelRoute) {
        self.efforts_cache.invalidate(route);
    }

    /// Fetch the catalog now and cache it (tests; production fills it in the background).
    #[cfg(test)]
    pub(crate) async fn warm_efforts(&self, route: &ModelRoute) {
        if let Ok(provider) = self.providers.resolve(route).await {
            fetch_into(&self.efforts_cache, provider.as_ref(), route).await;
        }
    }
}

//! Refresh coordination: single-flight concurrency de-duplication and terminal failure tracking.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

use crate::error::{AuthError, Result};

#[derive(Clone)]
enum FlightOutcome<T> {
    Ok(T),
    Err(String),
}

type FlightReceiver<T> = watch::Receiver<Option<FlightOutcome<T>>>;
type FlightMap<T> = HashMap<String, FlightReceiver<T>>;

/// Coordinates concurrent token refresh operations to ensure that multiple simultaneous
/// requests for the same credential run only once and share the result.
#[derive(Clone, Default)]
pub struct SingleFlight<T = String> {
    flights: Arc<Mutex<FlightMap<T>>>,
}

impl<T: Clone + Send + Sync + 'static> SingleFlight<T> {
    /// Create a new `SingleFlight` coordinator.
    pub fn new() -> Self {
        Self {
            flights: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Execute `f` for `key` if no flight is currently in progress.
    ///
    /// If another caller is already executing for the same `key`, awaits its result
    /// and returns a clone of that outcome without re-executing `f`.
    pub async fn run<F, Fut>(&self, key: &str, f: F) -> Result<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T>>,
    {
        let (tx, mut rx) = {
            let mut map = self
                .flights
                .lock()
                .map_err(|_| AuthError::Refresh("single flight mutex poisoned".into()))?;

            if let Some(existing_rx) = map.get(key) {
                (None, existing_rx.clone())
            } else {
                let (tx, rx) = watch::channel(None);
                map.insert(key.to_string(), rx.clone());
                (Some(tx), rx)
            }
        };

        if let Some(tx) = tx {
            // Leader task: executes the closure and publishes outcome to watchers
            struct CleanupGuard<'a, V> {
                flights: &'a Mutex<FlightMap<V>>,
                key: &'a str,
            }

            impl<'a, V> Drop for CleanupGuard<'a, V> {
                fn drop(&mut self) {
                    if let Ok(mut map) = self.flights.lock() {
                        map.remove(self.key);
                    }
                }
            }

            let guard = CleanupGuard {
                flights: &self.flights,
                key,
            };

            let res = f().await;
            match &res {
                Ok(val) => {
                    let _ = tx.send(Some(FlightOutcome::Ok(val.clone())));
                }
                Err(err) => {
                    let _ = tx.send(Some(FlightOutcome::Err(format!("{err}"))));
                }
            }
            drop(guard);

            res
        } else {
            // Follower task: awaits leader's result
            match rx.wait_for(|val| val.is_some()).await {
                Ok(borrowed) => match borrowed.as_ref() {
                    Some(FlightOutcome::Ok(val)) => Ok(val.clone()),
                    Some(FlightOutcome::Err(msg)) => Err(AuthError::Refresh(msg.clone())),
                    None => Err(AuthError::Refresh("empty flight outcome".into())),
                },
                Err(_) => Err(AuthError::Refresh(
                    "refresh leader cancelled or dropped".into(),
                )),
            }
        }
    }
}

/// Tracks terminal refresh failures (e.g. `invalid_grant`) to prevent repeated
/// doomed requests to provider endpoints.
#[derive(Clone, Default)]
pub struct RefreshState {
    terminal_failures: Arc<Mutex<HashMap<String, String>>>,
}

impl RefreshState {
    /// Create a new `RefreshState` tracker.
    pub fn new() -> Self {
        Self {
            terminal_failures: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Check if refreshes are allowed for `key`.
    ///
    /// Returns `Err(AuthError::Refresh)` if a terminal failure was previously recorded.
    pub fn ensure_allowed(&self, key: &str) -> Result<()> {
        let map = self
            .terminal_failures
            .lock()
            .map_err(|_| AuthError::Refresh("refresh state mutex poisoned".into()))?;

        if let Some(reason) = map.get(key) {
            return Err(AuthError::Refresh(format!(
                "refresh disabled due to terminal failure for '{key}': {reason}"
            )));
        }

        Ok(())
    }

    /// Record the outcome of a refresh operation for `key`.
    ///
    /// If `ok_or_terminal` is `Ok(())`, clears any prior terminal failure.
    /// If `ok_or_terminal` is `Err(reason)`, records `reason` as a terminal failure.
    pub fn record_outcome(&self, key: &str, ok_or_terminal: std::result::Result<(), &str>) {
        if let Ok(mut map) = self.terminal_failures.lock() {
            match ok_or_terminal {
                Ok(()) => {
                    map.remove(key);
                }
                Err(reason) => {
                    map.insert(key.to_string(), reason.to_string());
                }
            }
        }
    }

    /// Clear any recorded terminal failure for `key`.
    pub fn clear(&self, key: &str) {
        self.record_outcome(key, Ok(()));
    }
}

#[cfg(test)]
#[path = "refresh_tests.rs"]
mod refresh_tests;

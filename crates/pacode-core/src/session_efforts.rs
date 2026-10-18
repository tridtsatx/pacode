//! Reasoning-effort clamping: an effort the model does not support never reaches
//! the session, whichever client asked for it (TUI, ACP, subagent inheritance).

use std::time::Duration;

use pacode_types::{Effort, ModelRoute};

use crate::session::Session;

/// How long a clamp waits for a provider catalog before falling back to its
/// static knowledge of the model.
const CATALOG_WAIT: Duration = Duration::from_secs(2);

impl Session {
    /// Reasoning levels `route` supports; empty when the model has no reasoning
    /// control or nothing is known about it.
    pub(crate) async fn supported_efforts(&self, route: &ModelRoute) -> Vec<Effort> {
        let Ok(provider) = self.providers.resolve(route).await else {
            return Vec::new();
        };
        let listed = tokio::time::timeout(CATALOG_WAIT, provider.list_models())
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|all| all.into_iter().find(|m| m.route.model == route.model));
        match listed {
            Some(info) => info.efforts,
            None => provider.model_info(&route.model).efforts,
        }
    }

    /// `effort` moved to the nearest level `route` supports (preferring the next
    /// lower one). Unchanged when the model's levels are unknown or it has none.
    pub(crate) async fn clamp_effort_for(&self, route: &ModelRoute, effort: Effort) -> Effort {
        let supported = self.supported_efforts(route).await;
        effort.clamp_to(&supported).unwrap_or(effort)
    }
}

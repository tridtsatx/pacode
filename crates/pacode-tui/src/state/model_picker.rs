//! State of the `/model` picker: the catalog grouped by provider, the kind and
//! search filters, and the ordered view the list draws. Everything the filter
//! needs (lowercase `provider/model` keys, provider groups, present kinds) is
//! computed once when the catalog arrives, so typing a query only scans
//! precomputed keys and allocates one small index vector.

use pacode_types::{AuthState, Effort, ModelInfo, ModelKind, ModelRoute, ProviderAuthInfo};

use crate::state::AppState;

/// Most models the picker indexes; anything past this is ignored.
pub const MAX_MODELS: usize = 20_000;
/// Recently used models pinned to the top of the list.
pub const MAX_RECENT: usize = 8;
/// Longest search query.
pub const MAX_QUERY: usize = 96;

/// What Enter assigns the highlighted model to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PickerRole {
    /// The session's main model.
    #[default]
    Main,
    /// `agents.default_model` / `agents.default_effort`.
    Agents,
    /// `context.compaction_model`.
    Compaction,
}

impl PickerRole {
    pub const ALL: [PickerRole; 3] = [PickerRole::Main, PickerRole::Agents, PickerRole::Compaction];

    pub fn label(self) -> &'static str {
        match self {
            PickerRole::Main => "default",
            PickerRole::Agents => "agents",
            PickerRole::Compaction => "compaction",
        }
    }

    pub fn next(self) -> PickerRole {
        match self {
            PickerRole::Main => PickerRole::Agents,
            PickerRole::Agents => PickerRole::Compaction,
            PickerRole::Compaction => PickerRole::Main,
        }
    }
}

/// One catalog model, with the key the filter scans.
#[derive(Clone, Debug)]
pub struct Entry {
    /// Index into `AppState::models`.
    pub model: usize,
    /// Lowercase `provider/model`.
    pub key: String,
    /// Index into `ModelPickerState::groups`.
    pub group: usize,
    /// Position in the recent list (0 = most recent).
    pub recent: Option<usize>,
}

/// One provider in the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    pub configured: bool,
    pub count: usize,
}

/// Sidebar cursor positions before the provider rows.
pub const SIDE_ROLES: usize = 0;
pub const SIDE_ALL: usize = 1;
const SIDE_FIRST_PROVIDER: usize = 2;

#[derive(Clone, Debug, Default)]
pub struct ModelPickerState {
    pub entries: Vec<Entry>,
    pub groups: Vec<Group>,
    /// Kinds present in the catalog, in display order.
    pub kinds: Vec<ModelKind>,
    /// Active kind filter; `None` = all.
    pub kind: Option<ModelKind>,
    /// Sidebar cursor: `SIDE_ROLES`, `SIDE_ALL`, or `2 + group index`.
    pub side: usize,
    pub query: String,
    /// Entry indices in display order (empty on the Roles view).
    pub view: Vec<u32>,
    /// Leading rows of `view` that are recently used models.
    pub recent_rows: usize,
    /// Highlighted row: an index into `view`, or the role row on the Roles view.
    pub cursor: usize,
    /// Role Enter assigns to.
    pub role: PickerRole,
    /// Recently used `provider/model` strings, newest first.
    pub recent: Vec<String>,
}

impl ModelPickerState {
    /// Index the catalog. Keeps the sidebar position, kind and query where they
    /// still make sense, so a catalog refresh under an open picker does not jump.
    pub fn rebuild(&mut self, models: &[ModelInfo], auth: &[ProviderAuthInfo]) {
        let kept_group = self
            .groups
            .get(self.side.wrapping_sub(SIDE_FIRST_PROVIDER))
            .map(|g| g.id.clone());

        let mut groups: Vec<Group> = Vec::new();
        for m in models.iter().take(MAX_MODELS) {
            match groups.iter_mut().find(|g| g.id == m.route.provider) {
                Some(g) => g.count += 1,
                None => groups.push(Group {
                    id: m.route.provider.clone(),
                    configured: true,
                    count: 1,
                }),
            }
        }
        for a in auth {
            if groups.iter().all(|g| g.id != a.id) {
                groups.push(Group {
                    id: a.id.clone(),
                    configured: matches!(a.state, AuthState::Configured),
                    count: 0,
                });
            }
        }
        // Configured providers first, each half alphabetical.
        groups.sort_by(|a, b| {
            b.configured
                .cmp(&a.configured)
                .then_with(|| a.id.cmp(&b.id))
        });

        let mut entries: Vec<Entry> = models
            .iter()
            .enumerate()
            .take(MAX_MODELS)
            .filter_map(|(i, m)| {
                let group = groups.iter().position(|g| g.id == m.route.provider)?;
                Some(Entry {
                    model: i,
                    key: m.route.to_string().to_lowercase(),
                    group,
                    recent: None,
                })
            })
            .collect();
        entries.sort_by(|a, b| a.key.cmp(&b.key));

        let mut kinds: Vec<ModelKind> = models.iter().take(MAX_MODELS).map(|m| m.kind).collect();
        kinds.sort();
        kinds.dedup();

        self.entries = entries;
        self.groups = groups;
        self.kinds = kinds;
        if self.kind.is_some_and(|k| !self.kinds.contains(&k)) {
            self.kind = None;
        }
        self.side = match kept_group {
            Some(id) => self
                .groups
                .iter()
                .position(|g| g.id == id)
                .map_or(SIDE_ALL, |p| p + SIDE_FIRST_PROVIDER),
            None => self.side.min(SIDE_ALL),
        };
        self.apply_recent();
        self.recompute(models);
    }

    /// Replace the recent list (capped) and re-mark the entries.
    pub fn set_recent(&mut self, recent: Vec<String>) {
        self.recent = recent;
        self.recent.truncate(MAX_RECENT);
        self.apply_recent();
    }

    /// Move `route` to the front of the recent list.
    pub fn touch_recent(&mut self, route: &ModelRoute) {
        let s = route.to_string();
        self.recent.retain(|r| *r != s);
        self.recent.insert(0, s);
        self.recent.truncate(MAX_RECENT);
        self.apply_recent();
    }

    fn apply_recent(&mut self) {
        let recent: Vec<String> = self.recent.iter().map(|r| r.to_lowercase()).collect();
        for e in &mut self.entries {
            e.recent = recent.iter().position(|r| *r == e.key);
        }
    }

    /// Rebuild `view` from the sidebar position, kind and query.
    pub fn recompute(&mut self, models: &[ModelInfo]) {
        self.view.clear();
        self.recent_rows = 0;
        self.cursor = 0;
        if self.side == SIDE_ROLES {
            return;
        }
        let group_filter = self.side.checked_sub(SIDE_FIRST_PROVIDER);
        let tokens: Vec<String> = self
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        let candidates = self.entries.iter().enumerate().filter(|(_, e)| {
            group_filter.is_none_or(|g| e.group == g)
                && self
                    .kind
                    .is_none_or(|k| models.get(e.model).is_some_and(|m| m.kind == k))
        });

        if tokens.is_empty() {
            let mut recents: Vec<(usize, u32)> = Vec::new();
            for (i, e) in candidates {
                match e.recent {
                    Some(rank) => recents.push((rank, i as u32)),
                    None => self.view.push(i as u32),
                }
            }
            recents.sort_unstable();
            self.recent_rows = recents.len();
            let mut out: Vec<u32> = recents.into_iter().map(|(_, i)| i).collect();
            out.append(&mut self.view);
            self.view = out;
        } else {
            let mut scored: Vec<(i32, u32)> = candidates
                .filter_map(|(i, e)| {
                    let mut total = 0i32;
                    for t in &tokens {
                        total += fuzzy_score(t, &e.key)?;
                    }
                    Some((total, i as u32))
                })
                .collect();
            // Best score first; entries are key-sorted, so the index breaks ties.
            scored.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.view = scored.into_iter().map(|(_, i)| i).collect();
        }
    }

    /// Number of rows the cursor moves over in the current view.
    pub fn row_count(&self) -> usize {
        if self.side == SIDE_ROLES {
            PickerRole::ALL.len()
        } else {
            self.view.len()
        }
    }

    pub fn move_cursor(&mut self, delta: isize) {
        let n = self.row_count();
        if n == 0 {
            self.cursor = 0;
            return;
        }
        self.cursor = self.cursor.saturating_add_signed(delta).min(n - 1);
    }

    pub fn move_side(&mut self, delta: isize, models: &[ModelInfo]) {
        let last = (SIDE_FIRST_PROVIDER + self.groups.len())
            .saturating_sub(1)
            .max(SIDE_ALL);
        let next = self.side.saturating_add_signed(delta).min(last);
        if next != self.side {
            self.side = next;
            self.recompute(models);
        }
    }

    /// Step the kind filter through `all, <present kinds…>`.
    pub fn cycle_kind(&mut self, delta: isize, models: &[ModelInfo]) {
        let n = self.kinds.len() + 1;
        let cur = self
            .kind
            .and_then(|k| self.kinds.iter().position(|x| *x == k))
            .map_or(0, |p| p + 1);
        let next = (cur as isize + delta).rem_euclid(n as isize) as usize;
        self.kind = next.checked_sub(1).and_then(|p| self.kinds.get(p).copied());
        self.recompute(models);
    }

    pub fn push_query(&mut self, c: char, models: &[ModelInfo]) {
        if self.query.chars().count() < MAX_QUERY {
            self.query.push(c);
            self.recompute(models);
        }
    }

    /// Append pasted text (already single-line) and re-filter once.
    pub fn append_query(&mut self, text: &str, models: &[ModelInfo]) {
        let room = MAX_QUERY.saturating_sub(self.query.chars().count());
        let add: String = text
            .chars()
            .filter(|c| !c.is_control())
            .take(room)
            .collect();
        if !add.is_empty() {
            self.query.push_str(&add);
            self.recompute(models);
        }
    }

    pub fn pop_query(&mut self, models: &[ModelInfo]) {
        if self.query.pop().is_some() {
            self.recompute(models);
        }
    }

    /// The highlighted model.
    pub fn highlighted<'a>(&self, models: &'a [ModelInfo]) -> Option<&'a ModelInfo> {
        if self.side == SIDE_ROLES {
            return None;
        }
        let entry = self.entries.get(*self.view.get(self.cursor)? as usize)?;
        models.get(entry.model)
    }

    /// Put the cursor on `route` when it is in the view.
    pub fn select_route(&mut self, route: &ModelRoute, models: &[ModelInfo]) {
        let found = self.view.iter().position(|&i| {
            self.entries
                .get(i as usize)
                .and_then(|e| models.get(e.model))
                .is_some_and(|m| m.route == *route)
        });
        if let Some(p) = found {
            self.cursor = p;
        }
    }
}

/// Score of `needle` (lowercase) against `hay` (lowercase): `None` when `needle`
/// is not a subsequence. A contiguous match outranks a scattered one; word starts
/// and early matches score higher. Allocation-free.
pub fn fuzzy_score(needle: &str, hay: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let h = hay.as_bytes();
    if let Some(pos) = hay.find(needle) {
        let boundary = pos == 0 || !h[pos - 1].is_ascii_alphanumeric();
        return Some(1000 + if boundary { 200 } else { 0 } - (pos as i32).min(500));
    }
    let mut score = 0i32;
    let mut at = 0usize;
    let mut prev: Option<usize> = None;
    for &c in needle.as_bytes() {
        let mut found = None;
        while at < h.len() {
            if h[at] == c {
                found = Some(at);
                at += 1;
                break;
            }
            at += 1;
        }
        let p = found?;
        score += 10;
        if p == 0 || !h[p - 1].is_ascii_alphanumeric() {
            score += 10;
        }
        match prev {
            Some(q) if q + 1 == p => score += 15,
            Some(q) => score -= ((p - q - 1) as i32).min(10),
            None => score -= (p as i32).min(10),
        }
        prev = Some(p);
    }
    Some(score)
}

impl AppState {
    /// Catalog entry of the session's current model, once the catalog is known.
    pub fn model_info(&self) -> Option<&ModelInfo> {
        let route = self.model()?;
        self.models.iter().find(|m| m.route == *route)
    }

    /// Reasoning levels the current model offers. `None` while the catalog does
    /// not list the model (nothing known, so nothing is hidden or clamped).
    pub fn supported_efforts(&self) -> Option<&[Effort]> {
        self.model_info().map(|m| m.efforts.as_slice())
    }

    /// Whether the footer and `/effort` show an effort for the current model.
    pub fn effort_visible(&self) -> bool {
        self.supported_efforts().is_none_or(|e| !e.is_empty())
    }

    /// The model a role currently points at; roles with no setting follow `default`.
    pub fn role_target(&self, role: PickerRole) -> Option<ModelRoute> {
        let configured = match role {
            PickerRole::Main => None,
            PickerRole::Agents => self.config.agents.default_model.as_deref(),
            PickerRole::Compaction => self.config.context.compaction_model.as_deref(),
        };
        configured
            .and_then(ModelRoute::parse_lossy)
            .or_else(|| self.model().cloned())
    }

    /// Whether the role has a model of its own in the config.
    pub fn role_is_set(&self, role: PickerRole) -> bool {
        match role {
            PickerRole::Main => true,
            PickerRole::Agents => self.config.agents.default_model.is_some(),
            PickerRole::Compaction => self.config.context.compaction_model.is_some(),
        }
    }

    /// Levels `/effort` offers: the model's own, or every level while the
    /// catalog does not list the model.
    pub fn effort_levels(&self) -> Vec<Effort> {
        match self.supported_efforts() {
            Some(e) if !e.is_empty() => e.to_vec(),
            _ => Effort::ALL.to_vec(),
        }
    }

    /// Re-index the picker after the catalog or the auth list changed.
    pub fn refresh_model_picker(&mut self) {
        let route = self.model().cloned();
        self.model_picker
            .rebuild(&self.models, &self.auth_providers);
        if let Some(r) = route
            && self.model_picker.query.is_empty()
            && matches!(
                self.focus,
                crate::state::Focus::Overlay(crate::state::Overlay::ModelPicker { .. })
            )
        {
            self.model_picker.select_route(&r, &self.models);
        }
    }
}

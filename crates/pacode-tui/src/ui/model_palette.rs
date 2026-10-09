//! The central model palette: one screen for picking a model, its effort and
//! the provider to sign in to. `/model` and `/login` both open it.
//!
//! ```text
//! ╭─ Select model ──────────────────────────────╮
//! │ › son█                                       │
//! │                                              │
//! │  RECENT                                      │
//! │ ▸ Claude Sonnet 5.5     200k  $3/$15         │
//! │                                              │
//! │  CLAUDE  ● signed in                         │
//! │   Claude Sonnet 5.5     200k  $3/$15         │
//! │                                              │
//! │  OPENROUTER  ○ enter to sign in              │
//! │ effort  low · [medium] · high · xhigh        │
//! │ ↑↓ move · enter select · tab effort · esc    │
//! ╰──────────────────────────────────────────────╯
//! ```
//!
//! Everything on screen is a pure function of [`Sources`] plus the palette's
//! own fields, so a keypress, a repaint and a test read the same rows. The
//! effort the palette shows and sends is *derived* from the highlighted model
//! ([`ModelPalette::effort_for`]) rather than stored per model, so no path —
//! typing, moving, a catalog refresh — can leave it on a level that model
//! rejects.

use pacode_types::{AuthState, Effort, ModelInfo, ModelRoute, ProviderAuthInfo};

use crate::state::AppState;
use crate::ui::model_catalog::{Flags, Kind, fuzzy_score, next_effort, prev_effort, snap_effort};

/// How many recently used routes the `RECENT` group shows (and `prefs.toml`
/// keeps).
pub const RECENT_CAP: usize = 5;

/// What the palette reads from the app. Borrowed, so building rows costs no
/// copy of the catalog.
pub struct Sources<'a> {
    pub models: &'a [ModelInfo],
    pub flags: &'a Flags,
    pub auth: &'a [ProviderAuthInfo],
    /// Most recent first, `provider/model` keys.
    pub recent: &'a [String],
}

impl<'a> Sources<'a> {
    pub fn of(state: &'a AppState) -> Self {
        Self {
            models: &state.models,
            flags: &state.model_flags,
            auth: &state.auth_providers,
            recent: &state.recent_models,
        }
    }

    fn auth_for(&self, provider: &str) -> Option<&'a ProviderAuthInfo> {
        self.auth.iter().find(|info| info.id == provider)
    }
}

/// A group header's auth status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupStatus {
    /// `RECENT`, or a provider with no auth record.
    Plain,
    SignedIn,
    SignIn,
    Error(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Row<'a> {
    /// Spacer between groups.
    Gap,
    Header {
        title: String,
        /// The provider (auth) id; `None` for `RECENT`.
        provider: Option<String>,
        status: GroupStatus,
        /// How the provider signs in (`OAuth`, `API key`, `local`), when it
        /// has an auth record.
        kind: Option<&'static str>,
    },
    Model {
        model: &'a ModelInfo,
        recent: bool,
        signed_in: bool,
    },
}

impl Row<'_> {
    /// Models, and the header of every provider with an auth record: Enter
    /// there signs in (another account, when already signed in), and the
    /// account and logout chords act on it.
    pub fn selectable(&self) -> bool {
        match self {
            Row::Gap => false,
            Row::Model { .. } => true,
            Row::Header {
                provider, status, ..
            } => provider.is_some() && *status != GroupStatus::Plain,
        }
    }

    /// The auth/provider id this row belongs to.
    pub fn provider(&self) -> Option<&str> {
        match self {
            Row::Gap => None,
            Row::Header { provider, .. } => provider.as_deref(),
            Row::Model { model, .. } => Some(&model.route.provider),
        }
    }
}

/// Where the selection lands once the row exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// `/model`: the session's route.
    Route(ModelRoute),
    /// `/login`: the first provider that needs a sign-in.
    FirstSignIn,
    /// A sign-in finished: the provider's first model once its catalog
    /// lands, its header until then.
    Provider(String),
}

/// Inline progress of a sign-in started from the palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginLine {
    pub provider: String,
    pub text: String,
    pub failed: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelPalette {
    pub query: String,
    pub kind: Kind,
    /// Index among the *selectable* rows, clamped at use.
    pub cursor: usize,
    /// A row to land on once it exists. Rows may still be loading when the
    /// palette opens; the first move or keystroke drops it.
    anchor: Option<Anchor>,
    /// The effort the user asked for. Shown and sent snapped to the
    /// highlighted model's ladder — see [`ModelPalette::effort_for`].
    pub effort: Effort,
    /// Only `zdr_capable` routes: ZDR active, or the Alt+Z enable flow.
    pub zdr_only: bool,
    /// Opened by Alt+Z: Enter also turns ZDR on, after the route switch.
    pub for_zdr: bool,
    pub login: Option<LoginLine>,
    /// The provider whose logout chord was pressed once; the next press
    /// signs it out, any other key forgets it.
    pub confirm_logout: Option<String>,
}

impl ModelPalette {
    pub fn new(effort: Effort, zdr_only: bool, for_zdr: bool, anchor: Option<Anchor>) -> Self {
        Self {
            effort,
            zdr_only: zdr_only || for_zdr,
            for_zdr,
            anchor,
            ..Self::default()
        }
    }

    /// Every row on screen, in order: `RECENT` (the recent routes that match),
    /// then one group per provider — signed-in first, then the ones that need
    /// a sign-in; within each, groups with models before the ones whose
    /// catalog is empty. Sign-in headers ignore the ZDR and kind filters:
    /// they list no models.
    pub fn rows<'a>(&self, src: &Sources<'a>) -> Vec<Row<'a>> {
        let query = self.query.trim();
        let mut groups: Vec<Group<'a>> = Vec::new();
        for model in src.models {
            if !self.kind.keeps(model) {
                continue;
            }
            // One key and one flag lookup per model, reused by the filter,
            // the score, the sort and `RECENT`.
            let key = model.route.to_string();
            let flags = src.flags.get(&key).copied().unwrap_or_default();
            if self.zdr_only && !flags.zdr_capable {
                continue;
            }
            let provider = model.route.provider.as_str();
            let index = match groups.iter().position(|g| g.provider == provider) {
                Some(index) => index,
                None => {
                    let auth = src.auth_for(provider);
                    groups.push(Group {
                        provider,
                        auth,
                        signed_in: auth.is_none_or(|info| info.state == AuthState::Configured),
                        models: Vec::new(),
                    });
                    groups.len() - 1
                }
            };
            let group = &mut groups[index];
            let label = group
                .auth
                .map_or(provider, |info| info.display_name.as_str());
            let Some(score) = [model.display_name.as_str(), key.as_str(), label]
                .iter()
                .filter_map(|text| fuzzy_score(query, text))
                .max()
            else {
                continue;
            };
            group.models.push(Entry {
                model,
                key,
                score,
                favorite: flags.favorite,
            });
        }
        groups.retain(|g| !g.models.is_empty());
        // Providers with an auth record but nothing listed: not signed in yet,
        // or signed in with an empty catalog. Matched on what the old login
        // screen searched.
        for info in src.auth {
            if groups.iter().any(|g| g.provider == info.id) {
                continue;
            }
            let matches = [&info.display_name, &info.id, &info.auth_kind, &info.detail]
                .iter()
                .any(|text| fuzzy_score(query, text).is_some());
            if matches {
                groups.push(Group {
                    provider: &info.id,
                    auth: Some(info),
                    signed_in: info.state == AuthState::Configured,
                    models: Vec::new(),
                });
            }
        }
        for group in &mut groups {
            group.models.sort_by(|a, b| {
                b.score
                    .cmp(&a.score)
                    .then_with(|| b.favorite.cmp(&a.favorite))
                    .then_with(|| a.model.display_name.cmp(&b.model.display_name))
                    .then_with(|| a.model.route.model.cmp(&b.model.route.model))
            });
        }
        groups.sort_by(|a, b| {
            let best = |g: &Group| g.models.first().map_or(i32::MIN, |e| e.score);
            b.signed_in
                .cmp(&a.signed_in)
                .then_with(|| a.models.is_empty().cmp(&b.models.is_empty()))
                .then_with(|| best(b).cmp(&best(a)))
                .then_with(|| {
                    let recommended = |g: &Group| g.auth.is_some_and(|info| info.recommended);
                    recommended(b).cmp(&recommended(a))
                })
                .then_with(|| a.provider.cmp(b.provider))
        });

        let mut rows = Vec::new();
        let recent: Vec<(&'a ModelInfo, bool)> = src
            .recent
            .iter()
            .filter_map(|key| {
                groups.iter().find_map(|g| {
                    g.models
                        .iter()
                        .find(|e| e.key == *key)
                        .map(|e| (e.model, g.signed_in))
                })
            })
            .take(RECENT_CAP)
            .collect();
        if !recent.is_empty() {
            rows.push(Row::Header {
                title: "RECENT".to_string(),
                provider: None,
                status: GroupStatus::Plain,
                kind: None,
            });
            rows.extend(recent.into_iter().map(|(model, signed_in)| Row::Model {
                model,
                recent: true,
                signed_in,
            }));
        }
        for group in &groups {
            push_gap(&mut rows);
            rows.push(Row::Header {
                title: group
                    .auth
                    .map_or(group.provider, |info| info.display_name.as_str())
                    .to_uppercase(),
                provider: Some(group.provider.to_string()),
                status: group.auth.map_or(GroupStatus::Plain, status_of),
                kind: group.auth.map(|info| kind_label(&info.auth_kind)),
            });
            rows.extend(group.models.iter().map(|entry| Row::Model {
                model: entry.model,
                recent: false,
                signed_in: group.signed_in,
            }));
        }
        rows
    }

    /// The row index of the selection: the anchor while it is pending and
    /// present, else the `cursor`-th selectable row (clamped).
    pub fn selected_index(&self, rows: &[Row]) -> Option<usize> {
        if let Some(index) = self
            .anchor
            .as_ref()
            .and_then(|anchor| anchor_index(anchor, rows))
        {
            return Some(index);
        }
        let selectable: Vec<usize> = selectable(rows).collect();
        selectable
            .get(self.cursor.min(selectable.len().saturating_sub(1)))
            .copied()
    }

    pub fn selected<'r, 'a>(&self, rows: &'r [Row<'a>]) -> Option<&'r Row<'a>> {
        self.selected_index(rows).and_then(|index| rows.get(index))
    }

    /// Move the selection by `delta` selectable rows, clamped to the list.
    pub fn move_by(&mut self, delta: isize, rows: &[Row]) {
        let selectable: Vec<usize> = selectable(rows).collect();
        let current = self
            .selected_index(rows)
            .and_then(|index| selectable.iter().position(|i| *i == index))
            .unwrap_or(0);
        let last = selectable.len().saturating_sub(1) as isize;
        self.cursor = (current as isize + delta).clamp(0, last.max(0)) as usize;
        self.anchor = None;
    }

    /// The query changed: the best match is on top, so the selection goes
    /// there.
    fn reset_selection(&mut self) {
        self.cursor = 0;
        self.anchor = None;
    }

    pub fn push_char(&mut self, ch: char) {
        if self.query.chars().count() >= crate::paste::FILTER_MAX_CHARS {
            return;
        }
        self.query.push(ch);
        self.reset_selection();
    }

    pub fn push_str(&mut self, text: &str) {
        let room = crate::paste::FILTER_MAX_CHARS.saturating_sub(self.query.chars().count());
        self.query.extend(text.chars().take(room));
        self.reset_selection();
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.reset_selection();
    }

    /// A sign-in finished: follow the provider's group as it unlocks, rather
    /// than keep a position the new rows shifted.
    pub fn follow_provider(&mut self, provider: &str) {
        self.anchor = Some(Anchor::Provider(provider.to_string()));
    }

    pub fn cycle_kind(&mut self) {
        self.kind = self.kind.next();
        self.reset_selection();
    }

    /// The effort the palette shows and sends for `model`: the asked-for
    /// level snapped onto what the model accepts. `None` when it accepts
    /// nothing.
    pub fn effort_for(&self, model: &ModelInfo) -> Option<Effort> {
        snap_effort(model, self.effort)
    }

    /// Tab / shift-tab: step along the highlighted model's ladder. Local only —
    /// nothing reaches the daemon until Enter.
    pub fn cycle_effort(&mut self, forward: bool, model: &ModelInfo) {
        let Some(current) = self.effort_for(model) else {
            return;
        };
        let next = if forward {
            next_effort(model, current)
        } else {
            prev_effort(model, current)
        };
        if let Some(next) = next {
            self.effort = next;
        }
    }
}

struct Group<'a> {
    provider: &'a str,
    auth: Option<&'a ProviderAuthInfo>,
    signed_in: bool,
    models: Vec<Entry<'a>>,
}

struct Entry<'a> {
    model: &'a ModelInfo,
    key: String,
    score: i32,
    favorite: bool,
}

fn anchor_index(anchor: &Anchor, rows: &[Row]) -> Option<usize> {
    let header_of = |id: &str| {
        rows.iter()
            .position(|row| matches!(row, Row::Header { provider: Some(p), .. } if p == id))
    };
    match anchor {
        Anchor::Route(route) => rows
            .iter()
            .position(|row| matches!(row, Row::Model { model, .. } if model.route == *route)),
        Anchor::FirstSignIn => rows.iter().position(|row| {
            matches!(
                row,
                Row::Header {
                    status: GroupStatus::SignIn | GroupStatus::Error(_),
                    ..
                }
            )
        }),
        Anchor::Provider(id) => header_of(id).map(|header| {
            let first_model = matches!(
                rows.get(header + 1),
                Some(Row::Model { model, .. }) if model.route.provider == *id
            );
            if first_model { header + 1 } else { header }
        }),
    }
}

/// How a provider signs in, for its header.
fn kind_label(auth_kind: &str) -> &'static str {
    match auth_kind {
        "oauth" => "OAuth",
        "api_key" => "API key",
        "local" => "local",
        _ => "sign-in",
    }
}

fn status_of(info: &ProviderAuthInfo) -> GroupStatus {
    match &info.state {
        AuthState::Configured => GroupStatus::SignedIn,
        AuthState::NotConfigured => GroupStatus::SignIn,
        AuthState::NeedsAttention { reason } => GroupStatus::Error(reason.clone()),
    }
}

fn push_gap(rows: &mut Vec<Row>) {
    if !rows.is_empty() {
        rows.push(Row::Gap);
    }
}

fn selectable<'r>(rows: &'r [Row]) -> impl Iterator<Item = usize> + 'r {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| row.selectable())
        .map(|(index, _)| index)
}

#[cfg(test)]
#[path = "model_palette_tests.rs"]
mod model_palette_tests;

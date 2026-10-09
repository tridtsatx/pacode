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

use pacode_types::{AuthState, Effort, ModelInfo, ProviderAuthInfo};

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

    /// A provider whose models are listed is usable unless its auth record
    /// says otherwise; one with no record (a config-only provider) is usable.
    fn signed_in(&self, provider: &str) -> bool {
        self.auth_for(provider)
            .is_none_or(|info| info.state == AuthState::Configured)
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
        /// The auth id Enter signs in to; `None` for `RECENT`.
        provider: Option<String>,
        status: GroupStatus,
    },
    Model {
        model: &'a ModelInfo,
        recent: bool,
        signed_in: bool,
    },
}

impl Row<'_> {
    /// Models, and the headers of providers that need a sign-in.
    pub fn selectable(&self) -> bool {
        match self {
            Row::Gap => false,
            Row::Model { .. } => true,
            Row::Header { status, .. } => {
                matches!(status, GroupStatus::SignIn | GroupStatus::Error(_))
            }
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

/// Where the selection lands when the palette opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Anchor {
    /// `/model`: the session's route.
    Route(String),
    /// `/login`: the first provider that needs a sign-in.
    FirstSignIn,
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

    /// Every row on screen, in order: `RECENT` (empty search only), then one
    /// group per provider — signed-in first, then the ones that need a
    /// sign-in, with providers that list no models last.
    pub fn rows<'a>(&self, src: &Sources<'a>) -> Vec<Row<'a>> {
        let query = self.query.trim();
        let flag = |model: &ModelInfo| {
            src.flags
                .get(&model.route.to_string())
                .copied()
                .unwrap_or_default()
        };
        let mut groups: Vec<Group<'a>> = Vec::new();
        for model in src.models {
            if !self.kind.keeps(model) || (self.zdr_only && !flag(model).zdr_capable) {
                continue;
            }
            let provider = model.route.provider.as_str();
            let auth = src.auth_for(provider);
            let label = auth.map_or(provider, |info| info.display_name.as_str());
            let Some(score) = [model.display_name.as_str(), &model.route.to_string(), label]
                .iter()
                .filter_map(|text| fuzzy_score(query, text))
                .max()
            else {
                continue;
            };
            match groups.iter_mut().find(|g| g.provider == provider) {
                Some(group) => group.models.push((model, score)),
                None => groups.push(Group {
                    provider,
                    auth,
                    models: vec![(model, score)],
                }),
            }
        }
        for group in &mut groups {
            group.models.sort_by(|(a, sa), (b, sb)| {
                sb.cmp(sa)
                    .then_with(|| flag(b).favorite.cmp(&flag(a).favorite))
                    .then_with(|| a.display_name.cmp(&b.display_name))
                    .then_with(|| a.route.model.cmp(&b.route.model))
            });
        }
        groups.sort_by(|a, b| {
            let best = |g: &Group| g.models.first().map_or(0, |(_, s)| *s);
            src.signed_in(b.provider)
                .cmp(&src.signed_in(a.provider))
                .then_with(|| best(b).cmp(&best(a)))
                .then_with(|| a.provider.cmp(b.provider))
        });

        let mut rows = Vec::new();
        if query.is_empty() {
            let recent: Vec<&'a ModelInfo> = src
                .recent
                .iter()
                .filter_map(|key| {
                    groups
                        .iter()
                        .flat_map(|g| g.models.iter())
                        .find(|(m, _)| m.route.to_string() == *key)
                        .map(|(m, _)| *m)
                })
                .take(RECENT_CAP)
                .collect();
            if !recent.is_empty() {
                rows.push(Row::Header {
                    title: "RECENT".to_string(),
                    provider: None,
                    status: GroupStatus::Plain,
                });
                rows.extend(recent.into_iter().map(|model| Row::Model {
                    model,
                    recent: true,
                    signed_in: src.signed_in(&model.route.provider),
                }));
            }
        }
        for group in &groups {
            push_gap(&mut rows);
            let signed_in = src.signed_in(group.provider);
            rows.push(Row::Header {
                title: group
                    .auth
                    .map_or(group.provider, |info| info.display_name.as_str())
                    .to_uppercase(),
                provider: Some(group.provider.to_string()),
                status: group.auth.map_or(GroupStatus::Plain, status_of),
            });
            rows.extend(group.models.iter().map(|(model, _)| Row::Model {
                model,
                recent: false,
                signed_in,
            }));
        }
        // Providers with nothing listed yet: a sign-in is the only thing to
        // do there. Hidden under ZDR — a fresh provider has no capable marks.
        if !self.zdr_only {
            let mut bare: Vec<&ProviderAuthInfo> = src
                .auth
                .iter()
                .filter(|info| info.state != AuthState::Configured)
                .filter(|info| !groups.iter().any(|g| g.provider == info.id))
                .filter(|info| {
                    fuzzy_score(query, &info.display_name).is_some()
                        || fuzzy_score(query, &info.id).is_some()
                })
                .collect();
            bare.sort_by(|a, b| {
                b.recommended
                    .cmp(&a.recommended)
                    .then_with(|| a.display_name.cmp(&b.display_name))
            });
            for info in bare {
                push_gap(&mut rows);
                rows.push(Row::Header {
                    title: info.display_name.to_uppercase(),
                    provider: Some(info.id.clone()),
                    status: status_of(info),
                });
            }
        }
        rows
    }

    /// The row index of the selection: the anchor while it is pending and
    /// present, else the `cursor`-th selectable row (clamped).
    pub fn selected_index(&self, rows: &[Row]) -> Option<usize> {
        if let Some(index) = self.anchor.as_ref().and_then(|anchor| {
            rows.iter().position(|row| match (anchor, row) {
                (Anchor::Route(route), Row::Model { model, .. }) => {
                    model.route.to_string() == *route
                }
                (Anchor::FirstSignIn, Row::Header { .. }) => row.selectable(),
                (Anchor::Route(_) | Anchor::FirstSignIn, _) => false,
            })
        }) {
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
    models: Vec<(&'a ModelInfo, i32)>,
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

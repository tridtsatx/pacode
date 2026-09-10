//! Keybinding definitions, parser, formatter and keymap configuration (spec §6).

use std::collections::{BTreeMap, BTreeSet};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_types::KeysConfig;

#[cfg(test)]
#[path = "binding_tests.rs"]
mod binding_tests;

/// All rebindable actions in pacode-tui.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    FollowAgent,
    FilesOverlay,
    NextAgent,
    PrevAgent,
    SessionPicker,
    BgList,
    ScrollUp,
    ScrollDown,
    ScrollTop,
    ScrollBottom,
    Cancel,
    Submit,
    Newline,
    ClearInput,
    DeleteWordForward,
    DeleteWordBack,
    StopAgent,
    KillTask,
    CycleMode,
    CopySelection,
    SelectAgent1,
    SelectAgent2,
    SelectAgent3,
    SelectAgent4,
    SelectAgent5,
    SelectAgent6,
    SelectAgent7,
    SelectAgent8,
    SelectAgent9,
    SelectSession1,
    SelectSession2,
    SelectSession3,
    SelectSession4,
    SelectSession5,
    SelectSession6,
    SelectSession7,
    SelectSession8,
    SelectSession9,
    SubmitNow,
    RemoveQueued,
    ClearQueue,
    PasteImage,
    /// Paste from the system clipboard: an image when one is there, otherwise text.
    PasteClipboard,
}

impl Action {
    /// Stable snake_case TOML name for config serialization.
    pub fn name(self) -> &'static str {
        match self {
            Self::FollowAgent => "follow_agent",
            Self::FilesOverlay => "files_overlay",
            Self::NextAgent => "next_agent",
            Self::PrevAgent => "prev_agent",
            Self::SessionPicker => "session_picker",
            Self::BgList => "bg_list",
            Self::ScrollUp => "scroll_up",
            Self::ScrollDown => "scroll_down",
            Self::ScrollTop => "scroll_top",
            Self::ScrollBottom => "scroll_bottom",
            Self::Cancel => "cancel",
            Self::Submit => "submit",
            Self::Newline => "newline",
            Self::ClearInput => "clear_input",
            Self::DeleteWordForward => "delete_word_forward",
            Self::DeleteWordBack => "delete_word_back",
            Self::StopAgent => "stop_agent",
            Self::KillTask => "kill_task",
            Self::CycleMode => "cycle_mode",
            Self::CopySelection => "copy_selection",
            Self::SelectAgent1 => "select_agent_1",
            Self::SelectAgent2 => "select_agent_2",
            Self::SelectAgent3 => "select_agent_3",
            Self::SelectAgent4 => "select_agent_4",
            Self::SelectAgent5 => "select_agent_5",
            Self::SelectAgent6 => "select_agent_6",
            Self::SelectAgent7 => "select_agent_7",
            Self::SelectAgent8 => "select_agent_8",
            Self::SelectAgent9 => "select_agent_9",
            Self::SelectSession1 => "select_session_1",
            Self::SelectSession2 => "select_session_2",
            Self::SelectSession3 => "select_session_3",
            Self::SelectSession4 => "select_session_4",
            Self::SelectSession5 => "select_session_5",
            Self::SelectSession6 => "select_session_6",
            Self::SelectSession7 => "select_session_7",
            Self::SelectSession8 => "select_session_8",
            Self::SelectSession9 => "select_session_9",
            Self::SubmitNow => "submit_now",
            Self::RemoveQueued => "remove_queued",
            Self::ClearQueue => "clear_queue",
            Self::PasteImage => "paste_image",
            Self::PasteClipboard => "paste_clipboard",
        }
    }

    /// User-friendly description shown in the keybindings overlay.
    pub fn description(self) -> &'static str {
        match self {
            Self::FollowAgent => "Follow agent (auto-scroll)",
            Self::FilesOverlay => "Show touched files overlay",
            Self::NextAgent => "Select next agent / task",
            Self::PrevAgent => "Select previous agent / task",
            Self::SessionPicker => "Open session picker",
            Self::BgList => "Open background tasks list",
            Self::ScrollUp => "Scroll transcript up",
            Self::ScrollDown => "Scroll transcript down",
            Self::ScrollTop => "Scroll transcript to top",
            Self::ScrollBottom => "Scroll transcript to bottom",
            Self::Cancel => "Cancel / close layer / unfollow",
            Self::Submit => "Submit prompt / open panel",
            Self::Newline => "Insert newline into prompt",
            Self::ClearInput => "Clear input prompt",
            Self::DeleteWordForward => "Delete word forward",
            Self::DeleteWordBack => "Delete word backward",
            Self::StopAgent => "Stop running agent",
            Self::KillTask => "Kill background task",
            Self::CycleMode => "Cycle permission mode",
            Self::CopySelection => "Copy selected text",
            Self::SelectAgent1 => "Select agent 1 (main)",
            Self::SelectAgent2 => "Select agent 2",
            Self::SelectAgent3 => "Select agent 3",
            Self::SelectAgent4 => "Select agent 4",
            Self::SelectAgent5 => "Select agent 5",
            Self::SelectAgent6 => "Select agent 6",
            Self::SelectAgent7 => "Select agent 7",
            Self::SelectAgent8 => "Select agent 8",
            Self::SelectAgent9 => "Select agent 9",
            Self::SelectSession1 => "Select session slot 1",
            Self::SelectSession2 => "Select session slot 2",
            Self::SelectSession3 => "Select session slot 3",
            Self::SelectSession4 => "Select session slot 4",
            Self::SelectSession5 => "Select session slot 5",
            Self::SelectSession6 => "Select session slot 6",
            Self::SelectSession7 => "Select session slot 7",
            Self::SelectSession8 => "Select session slot 8",
            Self::SelectSession9 => "Select session slot 9",
            Self::SubmitNow => "Submit immediately (run turn in background)",
            Self::RemoveQueued => "Remove last queued prompt",
            Self::ClearQueue => "Clear prompt queue",
            Self::PasteImage => "Paste image from clipboard",
            Self::PasteClipboard => "Paste from clipboard (image or text)",
        }
    }
}

/// A combination of key code and modifier keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Binding {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BindingError {
    #[error("empty binding specification")]
    Empty,
    #[error("unknown modifier '{0}'")]
    UnknownModifier(String),
    #[error("unknown key '{0}'")]
    UnknownKey(String),
    #[error("missing key in binding specification")]
    MissingKey,
}

/// Parse a binding specification string, e.g. "alt+f", "ctrl+shift+k", "pgup", "enter", "esc", "f5", "space", "a".
pub fn parse_binding(spec: &str) -> Result<Binding, BindingError> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(BindingError::Empty);
    }

    let (mod_parts, key_str) = if trimmed == "+" {
        (Vec::new(), "+")
    } else if let Some(prefix) = trimmed.strip_suffix("++") {
        let parts = prefix.split('+').collect::<Vec<_>>();
        (parts, "+")
    } else {
        let parts: Vec<&str> = trimmed.split('+').collect();
        if parts.len() == 1 {
            (Vec::new(), parts[0])
        } else {
            let key = parts[parts.len() - 1];
            let mods = parts[..parts.len() - 1].to_vec();
            (mods, key)
        }
    };

    let mut mods = KeyModifiers::NONE;
    for m in mod_parts {
        let m_clean = m.trim().to_ascii_lowercase();
        if m_clean.is_empty() {
            return Err(BindingError::MissingKey);
        }
        match m_clean.as_str() {
            "ctrl" | "control" => mods.insert(KeyModifiers::CONTROL),
            "alt" => mods.insert(KeyModifiers::ALT),
            "shift" => mods.insert(KeyModifiers::SHIFT),
            "super" | "meta" | "cmd" => mods.insert(KeyModifiers::SUPER),
            other => return Err(BindingError::UnknownModifier(other.to_string())),
        }
    }

    let key_clean = key_str.trim();
    if key_clean.is_empty() {
        return Err(BindingError::MissingKey);
    }

    let key_lower = key_clean.to_ascii_lowercase();
    let code = match key_lower.as_str() {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "backspace" => KeyCode::Backspace,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "space" => KeyCode::Char(' '),
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "pageup" | "pgup" | "page_up" => KeyCode::PageUp,
        "pagedown" | "pgdn" | "page_down" => KeyCode::PageDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "+" => KeyCode::Char('+'),
        other => {
            if other.starts_with('f')
                && let Ok(num) = other[1..].parse::<u8>()
                && (1..=24).contains(&num)
            {
                KeyCode::F(num)
            } else if key_clean.chars().count() == 1 {
                let c = key_clean.chars().next().unwrap_or(' ');
                if c.is_ascii_uppercase() {
                    mods.insert(KeyModifiers::SHIFT);
                    KeyCode::Char(c.to_ascii_lowercase())
                } else {
                    KeyCode::Char(c)
                }
            } else {
                return Err(BindingError::UnknownKey(key_clean.to_string()));
            }
        }
    };

    Ok(Binding { code, mods })
}

/// Format a binding into a canonical string that round-trips with `parse_binding`.
pub fn format_binding(b: &Binding) -> String {
    let mut parts = Vec::new();
    if b.mods.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl");
    }
    if b.mods.contains(KeyModifiers::ALT) {
        parts.push("alt");
    }
    if b.mods.contains(KeyModifiers::SHIFT) {
        parts.push("shift");
    }
    if b.mods.contains(KeyModifiers::SUPER) {
        parts.push("super");
    }

    let key_str = match b.code {
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::BackTab => "backtab".to_string(),
        KeyCode::Delete => "delete".to_string(),
        KeyCode::Insert => "insert".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::PageUp => "pgup".to_string(),
        KeyCode::PageDown => "pgdn".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => format!("{c}"),
        _ => "unknown".to_string(),
    };

    parts.push(&key_str);
    parts.join("+")
}

/// Check if a `Binding` matches an incoming crossterm `KeyEvent`.
pub fn matches_key(binding: &Binding, key: KeyEvent) -> bool {
    let mask =
        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT | KeyModifiers::SUPER;
    let b_mods = binding.mods & mask;
    let k_mods = key.modifiers & mask;

    // Shift+Tab and BackTab compatibility across terminals
    let b_is_backtab = binding.code == KeyCode::BackTab
        || (binding.code == KeyCode::Tab && b_mods.contains(KeyModifiers::SHIFT));
    let k_is_backtab = key.code == KeyCode::BackTab
        || (key.code == KeyCode::Tab && k_mods.contains(KeyModifiers::SHIFT));
    if b_is_backtab && k_is_backtab {
        let no_shift = KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER;
        return (b_mods & no_shift) == (k_mods & no_shift);
    }

    match (binding.code, key.code) {
        (KeyCode::Char(bc), KeyCode::Char(kc)) => {
            if !bc.eq_ignore_ascii_case(&kc) {
                return false;
            }
            let b_shift = b_mods.contains(KeyModifiers::SHIFT);
            let k_shift = k_mods.contains(KeyModifiers::SHIFT) || kc.is_ascii_uppercase();
            if b_shift != k_shift {
                return false;
            }
            let no_shift = KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER;
            (b_mods & no_shift) == (k_mods & no_shift)
        }
        (b_code, k_code) if b_code == k_code => b_mods == k_mods,
        _ => false,
    }
}

pub const ACTION_NAMES: &[(&str, Action)] = &[
    ("follow_agent", Action::FollowAgent),
    ("files_overlay", Action::FilesOverlay),
    ("next_agent", Action::NextAgent),
    ("prev_agent", Action::PrevAgent),
    ("session_picker", Action::SessionPicker),
    ("bg_list", Action::BgList),
    ("scroll_up", Action::ScrollUp),
    ("scroll_down", Action::ScrollDown),
    ("scroll_top", Action::ScrollTop),
    ("scroll_bottom", Action::ScrollBottom),
    ("cancel", Action::Cancel),
    ("submit", Action::Submit),
    ("newline", Action::Newline),
    ("clear_input", Action::ClearInput),
    ("delete_word_forward", Action::DeleteWordForward),
    ("delete_word_back", Action::DeleteWordBack),
    ("stop_agent", Action::StopAgent),
    ("kill_task", Action::KillTask),
    ("cycle_mode", Action::CycleMode),
    ("copy_selection", Action::CopySelection),
    ("select_agent_1", Action::SelectAgent1),
    ("select_agent_2", Action::SelectAgent2),
    ("select_agent_3", Action::SelectAgent3),
    ("select_agent_4", Action::SelectAgent4),
    ("select_agent_5", Action::SelectAgent5),
    ("select_agent_6", Action::SelectAgent6),
    ("select_agent_7", Action::SelectAgent7),
    ("select_agent_8", Action::SelectAgent8),
    ("select_agent_9", Action::SelectAgent9),
    ("select_session_1", Action::SelectSession1),
    ("select_session_2", Action::SelectSession2),
    ("select_session_3", Action::SelectSession3),
    ("select_session_4", Action::SelectSession4),
    ("select_session_5", Action::SelectSession5),
    ("select_session_6", Action::SelectSession6),
    ("select_session_7", Action::SelectSession7),
    ("select_session_8", Action::SelectSession8),
    ("select_session_9", Action::SelectSession9),
    ("submit_now", Action::SubmitNow),
    ("remove_queued", Action::RemoveQueued),
    ("clear_queue", Action::ClearQueue),
    ("paste_image", Action::PasteImage),
    ("paste_clipboard", Action::PasteClipboard),
];

/// The active keymap mapping `Action` to a list of `Binding`s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keymap {
    bindings: BTreeMap<Action, Vec<Binding>>,
    overrides: BTreeSet<Action>,
}

impl Keymap {
    /// Return the default key bindings reproducing existing pacode-tui key bindings exactly.
    pub fn defaults() -> Self {
        let mut bindings = BTreeMap::new();
        bindings.insert(
            Action::FollowAgent,
            vec![Binding {
                code: KeyCode::Char('f'),
                mods: KeyModifiers::ALT,
            }],
        );
        bindings.insert(
            Action::FilesOverlay,
            vec![Binding {
                code: KeyCode::Char('b'),
                mods: KeyModifiers::ALT,
            }],
        );
        bindings.insert(
            Action::NextAgent,
            vec![
                Binding {
                    code: KeyCode::Down,
                    mods: KeyModifiers::ALT,
                },
                Binding {
                    code: KeyCode::Char('j'),
                    mods: KeyModifiers::CONTROL,
                },
            ],
        );
        bindings.insert(
            Action::PrevAgent,
            vec![
                Binding {
                    code: KeyCode::Up,
                    mods: KeyModifiers::ALT,
                },
                Binding {
                    code: KeyCode::Char('k'),
                    mods: KeyModifiers::CONTROL,
                },
            ],
        );
        bindings.insert(
            Action::SessionPicker,
            vec![Binding {
                code: KeyCode::Char('p'),
                mods: KeyModifiers::CONTROL,
            }],
        );
        bindings.insert(
            Action::BgList,
            vec![Binding {
                code: KeyCode::Char('.'),
                mods: KeyModifiers::NONE,
            }],
        );
        bindings.insert(
            Action::ScrollUp,
            vec![Binding {
                code: KeyCode::PageUp,
                mods: KeyModifiers::NONE,
            }],
        );
        bindings.insert(
            Action::ScrollDown,
            vec![Binding {
                code: KeyCode::PageDown,
                mods: KeyModifiers::NONE,
            }],
        );
        bindings.insert(
            Action::ScrollTop,
            vec![Binding {
                code: KeyCode::Home,
                mods: KeyModifiers::NONE,
            }],
        );
        bindings.insert(
            Action::ScrollBottom,
            vec![Binding {
                code: KeyCode::End,
                mods: KeyModifiers::NONE,
            }],
        );
        bindings.insert(
            Action::Cancel,
            vec![Binding {
                code: KeyCode::Esc,
                mods: KeyModifiers::NONE,
            }],
        );
        bindings.insert(
            Action::Submit,
            vec![Binding {
                code: KeyCode::Enter,
                mods: KeyModifiers::NONE,
            }],
        );
        bindings.insert(
            Action::Newline,
            vec![
                Binding {
                    code: KeyCode::Enter,
                    mods: KeyModifiers::SHIFT,
                },
                Binding {
                    code: KeyCode::Enter,
                    mods: KeyModifiers::ALT,
                },
            ],
        );
        bindings.insert(
            Action::ClearInput,
            vec![Binding {
                code: KeyCode::Char('u'),
                mods: KeyModifiers::CONTROL,
            }],
        );
        bindings.insert(
            Action::DeleteWordForward,
            vec![Binding {
                code: KeyCode::Delete,
                mods: KeyModifiers::ALT,
            }],
        );
        bindings.insert(
            Action::DeleteWordBack,
            vec![Binding {
                code: KeyCode::Char('w'),
                mods: KeyModifiers::CONTROL,
            }],
        );
        bindings.insert(
            Action::StopAgent,
            vec![Binding {
                code: KeyCode::Char('s'),
                mods: KeyModifiers::NONE,
            }],
        );
        bindings.insert(
            Action::KillTask,
            vec![Binding {
                code: KeyCode::Char('k'),
                mods: KeyModifiers::NONE,
            }],
        );
        bindings.insert(
            Action::CycleMode,
            vec![
                Binding {
                    code: KeyCode::Tab,
                    mods: KeyModifiers::SHIFT,
                },
                Binding {
                    code: KeyCode::BackTab,
                    mods: KeyModifiers::NONE,
                },
            ],
        );
        bindings.insert(
            Action::CopySelection,
            vec![Binding {
                code: KeyCode::Char('c'),
                mods: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            }],
        );
        let digits = ['1', '2', '3', '4', '5', '6', '7', '8', '9'];
        let agent_actions = [
            Action::SelectAgent1,
            Action::SelectAgent2,
            Action::SelectAgent3,
            Action::SelectAgent4,
            Action::SelectAgent5,
            Action::SelectAgent6,
            Action::SelectAgent7,
            Action::SelectAgent8,
            Action::SelectAgent9,
        ];
        for (ch, act) in digits.iter().zip(agent_actions.iter()) {
            bindings.insert(
                *act,
                vec![Binding {
                    code: KeyCode::Char(*ch),
                    mods: KeyModifiers::ALT,
                }],
            );
        }
        let session_actions = [
            Action::SelectSession1,
            Action::SelectSession2,
            Action::SelectSession3,
            Action::SelectSession4,
            Action::SelectSession5,
            Action::SelectSession6,
            Action::SelectSession7,
            Action::SelectSession8,
            Action::SelectSession9,
        ];
        for (ch, act) in digits.iter().zip(session_actions.iter()) {
            bindings.insert(
                *act,
                vec![Binding {
                    code: KeyCode::Char(*ch),
                    mods: KeyModifiers::CONTROL | KeyModifiers::ALT,
                }],
            );
        }

        bindings.insert(
            Action::SubmitNow,
            vec![Binding {
                code: KeyCode::Enter,
                mods: KeyModifiers::CONTROL,
            }],
        );
        bindings.insert(
            Action::RemoveQueued,
            vec![Binding {
                code: KeyCode::Char('q'),
                mods: KeyModifiers::ALT,
            }],
        );
        bindings.insert(
            Action::ClearQueue,
            vec![Binding {
                code: KeyCode::Char('q'),
                mods: KeyModifiers::ALT | KeyModifiers::SHIFT,
            }],
        );
        bindings.insert(
            Action::PasteImage,
            vec![Binding {
                code: KeyCode::Char('v'),
                mods: KeyModifiers::CONTROL | KeyModifiers::ALT,
            }],
        );
        // ctrl+v and, where the terminal forwards it, ctrl+shift+v: image first, text otherwise.
        bindings.insert(
            Action::PasteClipboard,
            vec![
                Binding {
                    code: KeyCode::Char('v'),
                    mods: KeyModifiers::CONTROL,
                },
                Binding {
                    code: KeyCode::Char('v'),
                    mods: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                },
                Binding {
                    code: KeyCode::Char('V'),
                    mods: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                },
            ],
        );

        Self {
            bindings,
            overrides: BTreeSet::new(),
        }
    }

    /// Build keymap from `KeysConfig`, returning warnings for unknown actions or invalid syntax.
    pub fn from_config(cfg: &KeysConfig) -> (Self, Vec<String>) {
        let mut keymap = Self::defaults();
        let mut warnings = Vec::new();

        for (action_name, spec) in &cfg.bindings {
            let matched_action = Self::action_names()
                .iter()
                .find(|(name, _)| *name == action_name.as_str())
                .map(|(_, action)| *action);

            match matched_action {
                Some(action) => match parse_binding(spec) {
                    Ok(binding) => {
                        keymap.set_binding(action, binding);
                    }
                    Err(err) => {
                        log::warn!(
                            "rejected invalid key binding from config for '{action_name}': '{spec}' ({err})"
                        );
                        warnings.push(format!(
                            "invalid binding for '{action_name}': '{spec}' ({err})"
                        ));
                    }
                },
                None => {
                    log::warn!("rejected unknown key action from config: '{action_name}'");
                    warnings.push(format!("unknown key action: '{action_name}'"));
                }
            }
        }

        (keymap, warnings)
    }

    /// Consult keymap for which action matches the given key event.
    pub fn action_for(&self, key: KeyEvent) -> Option<Action> {
        for &(_, action) in Self::action_names() {
            if let Some(bindings) = self.bindings.get(&action) {
                for b in bindings {
                    if matches_key(b, key) {
                        return Some(action);
                    }
                }
            }
        }
        None
    }

    /// Current bindings configured for the action.
    pub fn bindings_for(&self, action: Action) -> &[Binding] {
        self.bindings.get(&action).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Stable list of action names and action variants.
    pub fn action_names() -> &'static [(&'static str, Action)] {
        ACTION_NAMES
    }

    /// Override the binding for an action.
    pub fn set_binding(&mut self, action: Action, binding: Binding) {
        self.bindings.insert(action, vec![binding]);
        self.overrides.insert(action);
    }

    /// Reset an action to its default bindings.
    pub fn reset_default(&mut self, action: Action) {
        if let Some(def) = Self::defaults().bindings.remove(&action) {
            self.bindings.insert(action, def);
        }
        self.overrides.remove(&action);
    }

    /// Check if an action's binding has been customized.
    pub fn is_overridden(&self, action: Action) -> bool {
        self.overrides.contains(&action)
    }

    /// Check if a proposed binding conflicts with another action's binding.
    pub fn find_conflict(
        &self,
        candidate: &Binding,
        for_action: Action,
    ) -> Option<(&'static str, Action)> {
        for &(name, act) in Self::action_names() {
            if act == for_action {
                continue;
            }
            if let Some(bindings) = self.bindings.get(&act) {
                for b in bindings {
                    if b == candidate {
                        return Some((name, act));
                    }
                }
            }
        }
        None
    }
}

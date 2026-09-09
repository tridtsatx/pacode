use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_types::KeysConfig;

use super::*;

#[test]
fn test_parse_binding_valid() {
    assert_eq!(
        parse_binding("alt+f").unwrap(),
        Binding {
            code: KeyCode::Char('f'),
            mods: KeyModifiers::ALT,
        }
    );
    assert_eq!(
        parse_binding("ctrl+shift+k").unwrap(),
        Binding {
            code: KeyCode::Char('k'),
            mods: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        }
    );
    assert_eq!(
        parse_binding("pgup").unwrap(),
        Binding {
            code: KeyCode::PageUp,
            mods: KeyModifiers::NONE,
        }
    );
    assert_eq!(
        parse_binding("enter").unwrap(),
        Binding {
            code: KeyCode::Enter,
            mods: KeyModifiers::NONE,
        }
    );
    assert_eq!(
        parse_binding("esc").unwrap(),
        Binding {
            code: KeyCode::Esc,
            mods: KeyModifiers::NONE,
        }
    );
    assert_eq!(
        parse_binding("f5").unwrap(),
        Binding {
            code: KeyCode::F(5),
            mods: KeyModifiers::NONE,
        }
    );
    assert_eq!(
        parse_binding("space").unwrap(),
        Binding {
            code: KeyCode::Char(' '),
            mods: KeyModifiers::NONE,
        }
    );
    assert_eq!(
        parse_binding("a").unwrap(),
        Binding {
            code: KeyCode::Char('a'),
            mods: KeyModifiers::NONE,
        }
    );
    // Uppercase must set SHIFT and lowercase the char
    assert_eq!(
        parse_binding("A").unwrap(),
        Binding {
            code: KeyCode::Char('a'),
            mods: KeyModifiers::SHIFT,
        }
    );
    // Bare plus
    assert_eq!(
        parse_binding("+").unwrap(),
        Binding {
            code: KeyCode::Char('+'),
            mods: KeyModifiers::NONE,
        }
    );
    // Plus with modifier
    assert_eq!(
        parse_binding("ctrl++").unwrap(),
        Binding {
            code: KeyCode::Char('+'),
            mods: KeyModifiers::CONTROL,
        }
    );
    // Super + x
    assert_eq!(
        parse_binding("super+x").unwrap(),
        Binding {
            code: KeyCode::Char('x'),
            mods: KeyModifiers::SUPER,
        }
    );
    // Digits: alt+1..9 and ctrl+alt+1..9
    assert_eq!(
        parse_binding("alt+1").unwrap(),
        Binding {
            code: KeyCode::Char('1'),
            mods: KeyModifiers::ALT,
        }
    );
    assert_eq!(
        parse_binding("alt+9").unwrap(),
        Binding {
            code: KeyCode::Char('9'),
            mods: KeyModifiers::ALT,
        }
    );
    assert_eq!(
        parse_binding("ctrl+alt+1").unwrap(),
        Binding {
            code: KeyCode::Char('1'),
            mods: KeyModifiers::CONTROL | KeyModifiers::ALT,
        }
    );
    assert_eq!(
        parse_binding("ctrl+alt+9").unwrap(),
        Binding {
            code: KeyCode::Char('9'),
            mods: KeyModifiers::CONTROL | KeyModifiers::ALT,
        }
    );
}

#[test]
fn test_parse_binding_errors() {
    assert_eq!(parse_binding(""), Err(BindingError::Empty));
    assert_eq!(parse_binding("   "), Err(BindingError::Empty));
    assert_eq!(parse_binding("ctrl+"), Err(BindingError::MissingKey));
    assert_eq!(
        parse_binding("hyper+a"),
        Err(BindingError::UnknownModifier("hyper".to_string()))
    );
    assert_eq!(
        parse_binding("notakey"),
        Err(BindingError::UnknownKey("notakey".to_string()))
    );
}

#[test]
fn test_format_and_parse_roundtrip() {
    let test_bindings = [
        Binding {
            code: KeyCode::Enter,
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::Esc,
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::PageUp,
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::PageDown,
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::Home,
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::End,
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::Tab,
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::BackTab,
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::Delete,
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::Insert,
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::Left,
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::Right,
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::Up,
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::Down,
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::F(5),
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::Char(' '),
            mods: KeyModifiers::NONE,
        },
        Binding {
            code: KeyCode::Char('a'),
            mods: KeyModifiers::CONTROL,
        },
        Binding {
            code: KeyCode::Char('k'),
            mods: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        },
        Binding {
            code: KeyCode::Char('f'),
            mods: KeyModifiers::ALT,
        },
        Binding {
            code: KeyCode::Char('x'),
            mods: KeyModifiers::SUPER,
        },
        Binding {
            code: KeyCode::Char('+'),
            mods: KeyModifiers::CONTROL,
        },
        Binding {
            code: KeyCode::Char('+'),
            mods: KeyModifiers::NONE,
        },
    ];

    for binding in &test_bindings {
        let formatted = format_binding(binding);
        let parsed = parse_binding(&formatted);
        assert_eq!(
            parsed,
            Ok(*binding),
            "Round-trip failed for binding {binding:?} formatted as '{formatted}'"
        );
    }
}

#[test]
fn test_matches_key() {
    let binding_alt_f = Binding {
        code: KeyCode::Char('f'),
        mods: KeyModifiers::ALT,
    };

    // Exact match
    assert!(matches_key(
        &binding_alt_f,
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT)
    ));

    // Modifier mismatch rejected
    assert!(!matches_key(
        &binding_alt_f,
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)
    ));
    assert!(!matches_key(
        &binding_alt_f,
        KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE)
    ));
    assert!(!matches_key(
        &binding_alt_f,
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT)
    ));

    // Shift+Tab <-> BackTab equivalence in both directions
    let binding_backtab = Binding {
        code: KeyCode::BackTab,
        mods: KeyModifiers::NONE,
    };
    let binding_shift_tab = Binding {
        code: KeyCode::Tab,
        mods: KeyModifiers::SHIFT,
    };

    assert!(matches_key(
        &binding_backtab,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT)
    ));
    assert!(matches_key(
        &binding_backtab,
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE)
    ));
    assert!(matches_key(
        &binding_shift_tab,
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE)
    ));
    assert!(matches_key(
        &binding_shift_tab,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT)
    ));
    // Modifier mismatch with BackTab rejected
    assert!(!matches_key(
        &binding_backtab,
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::ALT)
    ));

    // Char('a') binding does NOT match a Char('A') key event and vice versa
    let binding_char_a = Binding {
        code: KeyCode::Char('a'),
        mods: KeyModifiers::NONE,
    };
    assert!(!matches_key(
        &binding_char_a,
        KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE)
    ));
    assert!(!matches_key(
        &binding_char_a,
        KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT)
    ));

    let binding_char_upper_a = parse_binding("A").unwrap(); // Char('a') with SHIFT
    assert!(!matches_key(
        &binding_char_upper_a,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)
    ));

    // A Char binding with SHIFT matches a key event whose code is the uppercase char even without an explicit SHIFT modifier
    assert!(matches_key(
        &binding_char_upper_a,
        KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE)
    ));
    assert!(matches_key(
        &binding_char_upper_a,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SHIFT)
    ));
    assert!(matches_key(
        &binding_char_upper_a,
        KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT)
    ));
}

#[test]
fn test_keymap_defaults_all_actions_bound() {
    let keymap = Keymap::defaults();
    for &(name, action) in Keymap::action_names() {
        let bindings = keymap.bindings_for(action);
        assert!(
            !bindings.is_empty(),
            "Action '{name}' ({action:?}) has no default bindings"
        );
    }
}

#[test]
fn test_keymap_from_config() {
    // Valid override replaces default and is_overridden is true
    let mut cfg = KeysConfig::default();
    cfg.bindings
        .insert("follow_agent".to_string(), "ctrl+g".to_string());
    let (keymap, warnings) = Keymap::from_config(&cfg);
    assert!(warnings.is_empty());
    assert!(keymap.is_overridden(Action::FollowAgent));
    assert_eq!(
        keymap.bindings_for(Action::FollowAgent),
        &[Binding {
            code: KeyCode::Char('g'),
            mods: KeyModifiers::CONTROL,
        }]
    );

    // Unknown action name yields exactly one warning and changes nothing
    let mut cfg_unknown = KeysConfig::default();
    cfg_unknown
        .bindings
        .insert("bogus_action".to_string(), "ctrl+g".to_string());
    let (keymap_unknown, warnings_unknown) = Keymap::from_config(&cfg_unknown);
    assert_eq!(warnings_unknown.len(), 1);
    assert!(warnings_unknown[0].contains("unknown key action: 'bogus_action'"));
    assert_eq!(
        keymap_unknown.bindings_for(Action::FollowAgent),
        Keymap::defaults().bindings_for(Action::FollowAgent)
    );

    // Unparseable spec yields exactly one warning and keeps the default
    let mut cfg_bad = KeysConfig::default();
    cfg_bad
        .bindings
        .insert("follow_agent".to_string(), "invalid++key".to_string());
    let (keymap_bad, warnings_bad) = Keymap::from_config(&cfg_bad);
    assert_eq!(warnings_bad.len(), 1);
    assert!(warnings_bad[0].contains("invalid binding for 'follow_agent'"));
    assert!(!keymap_bad.is_overridden(Action::FollowAgent));
    assert_eq!(
        keymap_bad.bindings_for(Action::FollowAgent),
        Keymap::defaults().bindings_for(Action::FollowAgent)
    );
}

#[test]
fn test_keymap_reset_default() {
    let mut keymap = Keymap::defaults();
    let override_binding = Binding {
        code: KeyCode::Char('g'),
        mods: KeyModifiers::CONTROL,
    };
    keymap.set_binding(Action::FollowAgent, override_binding);
    assert!(keymap.is_overridden(Action::FollowAgent));
    assert_eq!(
        keymap.bindings_for(Action::FollowAgent),
        &[override_binding]
    );

    keymap.reset_default(Action::FollowAgent);
    assert!(!keymap.is_overridden(Action::FollowAgent));
    assert_eq!(
        keymap.bindings_for(Action::FollowAgent),
        Keymap::defaults().bindings_for(Action::FollowAgent)
    );
}

#[test]
fn test_keymap_find_conflict() {
    let keymap = Keymap::defaults();
    let alt_f = Binding {
        code: KeyCode::Char('f'),
        mods: KeyModifiers::ALT,
    };

    // Collides with follow_agent when checking for another action
    assert_eq!(
        keymap.find_conflict(&alt_f, Action::SessionPicker),
        Some(("follow_agent", Action::FollowAgent))
    );

    // Returns None for the action's own binding
    assert_eq!(keymap.find_conflict(&alt_f, Action::FollowAgent), None);

    // Unbound key has no conflict
    let unbound = Binding {
        code: KeyCode::Char('z'),
        mods: KeyModifiers::ALT,
    };
    assert_eq!(keymap.find_conflict(&unbound, Action::FollowAgent), None);
}

#[test]
fn test_keymap_action_for() {
    let keymap = Keymap::defaults();

    // Resolves default bindings
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT)),
        Some(Action::FollowAgent)
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT)),
        Some(Action::FilesOverlay)
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
        Some(Action::SessionPicker)
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE)),
        None
    );
}

#[test]
fn test_rebind_select_agent_2_takes_effect() {
    let mut cfg = KeysConfig::default();
    cfg.bindings
        .insert("select_agent_2".to_string(), "ctrl+shift+2".to_string());
    let (keymap, warnings) = Keymap::from_config(&cfg);
    assert!(warnings.is_empty());
    assert!(keymap.is_overridden(Action::SelectAgent2));

    // New binding resolves to SelectAgent2
    let new_key = KeyEvent::new(
        KeyCode::Char('2'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert_eq!(keymap.action_for(new_key), Some(Action::SelectAgent2));

    // Old default binding (alt+2) no longer resolves to SelectAgent2
    let old_key = KeyEvent::new(KeyCode::Char('2'), KeyModifiers::ALT);
    assert_ne!(keymap.action_for(old_key), Some(Action::SelectAgent2));
}

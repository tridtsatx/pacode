use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pacode_types::KeysConfig;

use super::*;

#[test]
fn test_cycle_mode_default_keys_match() {
    let keymap = Keymap::defaults();
    // BackTab with and without SHIFT
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE)),
        Some(Action::CycleMode)
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
        Some(Action::CycleMode)
    );
    // Tab+SHIFT
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT)),
        Some(Action::CycleMode)
    );
    // alt+m fallback
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT)),
        Some(Action::CycleMode)
    );
    // Plain Tab, plain m and ctrl+backtab do not cycle
    assert_ne!(
        keymap.action_for(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        Some(Action::CycleMode)
    );
    assert_ne!(
        keymap.action_for(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE)),
        Some(Action::CycleMode)
    );
    assert_ne!(
        keymap.action_for(KeyEvent::new(KeyCode::BackTab, KeyModifiers::CONTROL)),
        Some(Action::CycleMode)
    );
}

#[test]
fn test_matches_key_backtab_with_shift_and_tab_shift() {
    let backtab = Binding {
        code: KeyCode::BackTab,
        mods: KeyModifiers::NONE,
    };
    let tab_shift = Binding {
        code: KeyCode::Tab,
        mods: KeyModifiers::SHIFT,
    };
    let backtab_shift = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
    let tab_shift_key = KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT);
    assert!(matches_key(&backtab, backtab_shift));
    assert!(matches_key(&tab_shift, backtab_shift));
    assert!(matches_key(&backtab, tab_shift_key));
    assert!(matches_key(&tab_shift, tab_shift_key));
    // Plain Tab is not a shift+tab
    assert!(!matches_key(
        &tab_shift,
        KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)
    ));
}

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
            code: KeyCode::Char('b'),
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
    let alt_b = Binding {
        code: KeyCode::Char('b'),
        mods: KeyModifiers::ALT,
    };

    // Collides with follow_agent when checking for another action
    assert_eq!(
        keymap.find_conflict(&alt_b, Action::SessionPicker),
        Some(("follow_agent", Action::FollowAgent))
    );

    // Returns None for the action's own binding
    assert_eq!(keymap.find_conflict(&alt_b, Action::FollowAgent), None);

    // Unbound key has no conflict
    let unbound = Binding {
        code: KeyCode::Char('y'),
        mods: KeyModifiers::ALT,
    };
    assert_eq!(keymap.find_conflict(&unbound, Action::FollowAgent), None);
}

#[test]
fn test_keymap_action_for() {
    let keymap = Keymap::defaults();

    // Resolves default bindings
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT)),
        Some(Action::FollowAgent)
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT)),
        Some(Action::FilesOverlay)
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)),
        Some(Action::SessionPicker)
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Delete, KeyModifiers::ALT)),
        Some(Action::DeleteLine)
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Delete, KeyModifiers::CONTROL)),
        Some(Action::DeleteWordForward)
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)),
        Some(Action::DeleteWordBack)
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

#[test]
fn test_picker_mark_actions_defaults_and_scope() {
    // §9.1: alt is the primary spelling, ctrl the fallback for terminals
    // that eat alt; both live in the registry, changeable from Keys.
    let keymap = Keymap::defaults();
    assert_eq!(
        keymap.bindings_for(Action::ModelPickerFavorite),
        &[
            Binding {
                code: KeyCode::Char('f'),
                mods: KeyModifiers::ALT,
            },
            Binding {
                code: KeyCode::Char('g'),
                mods: KeyModifiers::CONTROL,
            },
        ]
    );
    assert_eq!(
        keymap.bindings_for(Action::ModelPickerZdr),
        &[
            Binding {
                code: KeyCode::Char('z'),
                mods: KeyModifiers::ALT,
            },
            Binding {
                code: KeyCode::Char('y'),
                mods: KeyModifiers::CONTROL,
            },
        ]
    );
    assert_eq!(Action::ModelPickerFavorite.scope(), Scope::Picker);
    assert_eq!(Action::ModelPickerZdr.scope(), Scope::Picker);
    assert_eq!(Action::ModelPickerFavorite.name(), "model_picker_favorite");
    assert_eq!(Action::ModelPickerZdr.name(), "model_picker_zdr");
}

#[test]
fn test_shared_chords_resolve_to_their_global_owner() {
    // The picker duplicates alt+f/alt+z under its own scope — in Normal
    // focus the global actions must still win the shared ladder.
    let keymap = Keymap::defaults();
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT)),
        Some(Action::FilesOverlay)
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT)),
        Some(Action::ToggleZdr)
    );
}

#[test]
fn test_find_conflict_is_scope_aware() {
    let keymap = Keymap::defaults();
    let alt_f = Binding {
        code: KeyCode::Char('f'),
        mods: KeyModifiers::ALT,
    };

    // alt+f is held by files_overlay — but the picker scope never meets the
    // global ladder, so binding it on a picker action is not a conflict.
    assert_eq!(
        keymap.find_conflict(&alt_f, Action::ModelPickerFavorite),
        None
    );

    // Inside the picker scope the same chord does collide: alt+z is
    // model_picker_zdr's chord, so favorite cannot also claim it.
    let alt_z = Binding {
        code: KeyCode::Char('z'),
        mods: KeyModifiers::ALT,
    };
    assert_eq!(
        keymap.find_conflict(&alt_z, Action::ModelPickerFavorite),
        Some(("model_picker_zdr", Action::ModelPickerZdr))
    );
}

#[test]
fn test_queue_actions_present_in_defaults() {
    let keymap = Keymap::defaults();
    let submit_now_bindings = keymap.bindings_for(Action::SubmitNow);
    assert_eq!(
        submit_now_bindings,
        &[Binding {
            code: KeyCode::Enter,
            mods: KeyModifiers::CONTROL,
        }]
    );

    let remove_queued_bindings = keymap.bindings_for(Action::RemoveQueued);
    assert_eq!(
        remove_queued_bindings,
        &[Binding {
            code: KeyCode::Char('q'),
            mods: KeyModifiers::ALT,
        }]
    );

    let clear_queue_bindings = keymap.bindings_for(Action::ClearQueue);
    assert_eq!(
        clear_queue_bindings,
        &[Binding {
            code: KeyCode::Char('q'),
            mods: KeyModifiers::ALT | KeyModifiers::SHIFT,
        }]
    );

    let paste_image_bindings = keymap.bindings_for(Action::PasteImage);
    assert_eq!(
        paste_image_bindings,
        &[Binding {
            code: KeyCode::Char('v'),
            mods: KeyModifiers::CONTROL | KeyModifiers::ALT,
        }]
    );
}

#[test]
fn test_editing_actions_defaults_scope_and_resolution() {
    let keymap = Keymap::defaults();

    // ctrl+arrows: word jumps and whole-input boundaries.
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL)),
        Some(Action::MoveWordLeft)
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL)),
        Some(Action::MoveWordRight)
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL)),
        Some(Action::MoveStart)
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL)),
        Some(Action::MoveEnd)
    );

    // Delete chords: alt+delete kills the line, ctrl+delete the next word,
    // ctrl/alt+backspace the previous word (ctrl+w stays).
    assert_eq!(
        keymap.bindings_for(Action::DeleteWordBack),
        &[
            Binding {
                code: KeyCode::Char('w'),
                mods: KeyModifiers::CONTROL,
            },
            Binding {
                code: KeyCode::Backspace,
                mods: KeyModifiers::CONTROL,
            },
            Binding {
                code: KeyCode::Backspace,
                mods: KeyModifiers::ALT,
            },
        ]
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL)),
        Some(Action::DeleteWordBack)
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT)),
        Some(Action::DeleteWordBack)
    );

    // Plain arrows/backspace stay unbound — the code path owns them.
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
        None
    );
    assert_eq!(
        keymap.action_for(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        None
    );

    // All five are prompt-scoped and serialize to snake_case names.
    for (act, name) in [
        (Action::MoveWordLeft, "move_word_left"),
        (Action::MoveWordRight, "move_word_right"),
        (Action::MoveStart, "move_start"),
        (Action::MoveEnd, "move_end"),
        (Action::DeleteLine, "delete_line"),
    ] {
        assert_eq!(act.scope(), Scope::Prompt);
        assert_eq!(act.name(), name);
    }
}

#[test]
fn the_palette_types_and_compaction_chords_are_picker_chords() {
    let keymap = Keymap::defaults();
    for action in [Action::ModelPickerTypes, Action::ModelPickerCompaction] {
        assert_eq!(action.scope(), Scope::Picker);
        let bindings = keymap.bindings_for(action);
        assert!(!bindings.is_empty(), "{action:?} has a default");
        for binding in bindings {
            // Letters in the palette are search text.
            assert!(!binding.mods.is_empty(), "{action:?}: {binding:?}");
            assert_eq!(keymap.find_conflict(binding, action), None, "{action:?}");
        }
    }
    // `ctrl+c` interrupts and quits before any overlay sees it.
    assert!(
        !keymap
            .bindings_for(Action::ModelPickerCompaction)
            .iter()
            .any(|b| b.code == KeyCode::Char('c') && b.mods.contains(KeyModifiers::CONTROL))
    );
}

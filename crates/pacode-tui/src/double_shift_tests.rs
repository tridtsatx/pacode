//! §4.2: timing, reset rules and the `available` flag of `DoubleShift`.

use super::*;

fn shift_press(kind: KeyEventKind) -> KeyEvent {
    KeyEvent::new_with_kind(
        KeyCode::Modifier(ModifierKeyCode::LeftShift),
        KeyModifiers::SHIFT,
        kind,
    )
}

#[test]
fn two_taps_within_threshold_fire() {
    let mut d = DoubleShift::default();
    let t = Instant::now();
    assert!(!d.observe(shift_press(KeyEventKind::Press), t, 400, true));
    assert!(d.observe(
        shift_press(KeyEventKind::Press),
        t + std::time::Duration::from_millis(300),
        400,
        true,
    ));
}

#[test]
fn taps_past_threshold_do_not_fire() {
    let mut d = DoubleShift::default();
    let t = Instant::now();
    assert!(!d.observe(shift_press(KeyEventKind::Press), t, 400, true));
    assert!(!d.observe(
        shift_press(KeyEventKind::Press),
        t + std::time::Duration::from_millis(500),
        400,
        true,
    ));
    // The too-late tap still arms — a following quick tap completes a pair.
    assert!(d.observe(
        shift_press(KeyEventKind::Press),
        t + std::time::Duration::from_millis(600),
        400,
        true,
    ));
}

#[test]
fn a_key_between_the_taps_breaks_the_pair() {
    let mut d = DoubleShift::default();
    let t = Instant::now();
    assert!(!d.observe(shift_press(KeyEventKind::Press), t, 400, true));
    let a = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(!d.observe(a, t + std::time::Duration::from_millis(10), 400, true));
    assert!(!d.observe(
        shift_press(KeyEventKind::Press),
        t + std::time::Duration::from_millis(20),
        400,
        true,
    ));
}

#[test]
fn release_between_presses_does_not_break_the_pair() {
    let mut d = DoubleShift::default();
    let t = Instant::now();
    assert!(!d.observe(shift_press(KeyEventKind::Press), t, 400, true));
    assert!(!d.observe(
        shift_press(KeyEventKind::Release),
        t + std::time::Duration::from_millis(10),
        400,
        true,
    ));
    assert!(d.observe(
        shift_press(KeyEventKind::Press),
        t + std::time::Duration::from_millis(20),
        400,
        true,
    ));
}

#[test]
fn shift_held_with_another_modifier_is_not_a_tap() {
    let mut d = DoubleShift::default();
    let t = Instant::now();
    let chord = KeyEvent::new_with_kind(
        KeyCode::Modifier(ModifierKeyCode::LeftShift),
        KeyModifiers::SHIFT | KeyModifiers::CONTROL,
        KeyEventKind::Press,
    );
    assert!(!d.observe(chord, t, 400, true));
    // And it does not arm the pair either.
    assert!(!d.observe(
        shift_press(KeyEventKind::Press),
        t + std::time::Duration::from_millis(10),
        400,
        true,
    ));
}

#[test]
fn inactive_gate_resets_and_never_fires() {
    let mut d = DoubleShift::default();
    let t = Instant::now();
    assert!(!d.observe(shift_press(KeyEventKind::Press), t, 400, true));
    // Focus left `Normal`: any event resets the pending tap.
    assert!(!d.observe(shift_press(KeyEventKind::Press), t, 400, false));
    assert!(!d.observe(
        shift_press(KeyEventKind::Press),
        t + std::time::Duration::from_millis(10),
        400,
        true,
    ));
}

#[test]
fn a_pair_cannot_chain_a_third_tap() {
    let mut d = DoubleShift::default();
    let t = Instant::now();
    assert!(!d.observe(shift_press(KeyEventKind::Press), t, 400, true));
    assert!(d.observe(
        shift_press(KeyEventKind::Press),
        t + std::time::Duration::from_millis(10),
        400,
        true,
    ));
    // The pair is consumed — the next tap starts a fresh one.
    assert!(!d.observe(
        shift_press(KeyEventKind::Press),
        t + std::time::Duration::from_millis(20),
        400,
        true,
    ));
}

#[test]
fn seeing_a_shift_event_marks_the_gesture_available() {
    let mut d = DoubleShift::default();
    assert!(!d.available());
    let _ = d.observe(shift_press(KeyEventKind::Press), Instant::now(), 400, false);
    assert!(d.available());
}

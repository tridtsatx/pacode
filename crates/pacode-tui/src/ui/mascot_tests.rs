use super::*;

#[test]
fn every_sprite_is_rectangular_and_renders_to_half_height() {
    for kind in MascotKind::ALL {
        let sprite = kind.sprite(0);
        assert_eq!(sprite.pixels.len(), HEIGHT * 2);
        for row in sprite.pixels {
            assert_eq!(row.chars().count(), WIDTH, "ragged sprite row: {row}");
        }
        assert_eq!(render(kind, true).len(), HEIGHT);
    }
}

#[test]
fn transparent_pixels_render_as_spaces() {
    let lines = render(MascotKind::Pacman, true);
    let first: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        first.starts_with("  "),
        "leading pixels must be blank: {first}"
    );
}

#[test]
fn ghosts_and_pacman_differ() {
    let pac = render(MascotKind::Pacman, true);
    let ghost = render(MascotKind::Blinky, true);
    assert_ne!(
        pac.last().map(|l| l.spans.len()),
        None,
        "pacman must render"
    );
    let pac_txt: String = pac[2].spans.iter().map(|s| s.content.as_ref()).collect();
    let ghost_txt: String = ghost[2].spans.iter().map(|s| s.content.as_ref()).collect();
    assert_ne!(pac_txt, ghost_txt);
}

#[test]
fn every_frame_of_every_mascot_keeps_the_block_size() {
    for kind in MascotKind::ALL {
        for frame in 0..FRAMES {
            let sprite = kind.sprite(frame);
            assert_eq!(sprite.pixels.len(), HEIGHT * 2, "{kind:?} frame {frame}");
            for row in sprite.pixels {
                assert_eq!(row.chars().count(), WIDTH, "ragged row in {kind:?}: {row}");
            }
            let lines = render_frame(kind, frame, true);
            assert_eq!(lines.len(), HEIGHT);
            for line in &lines {
                let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
                assert_eq!(pacode_render::display_width(&text), WIDTH);
            }
        }
    }
}

#[test]
fn the_frames_actually_differ() {
    for kind in MascotKind::ALL {
        let a = kind.sprite(0).pixels;
        let b = kind.sprite(1).pixels;
        assert_ne!(a, b, "{kind:?} does not animate");
    }
}

#[test]
fn the_frame_counter_wraps_instead_of_running_out() {
    let a = render_frame(MascotKind::Pinky, 0, true);
    let b = render_frame(MascotKind::Pinky, FRAMES, true);
    assert_eq!(a.len(), b.len());
    let a0: String = a[0].spans.iter().map(|s| s.content.as_ref()).collect();
    let b0: String = b[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(a0, b0);
}

#[test]
fn the_ascii_fallback_animates_at_a_fixed_size() {
    for frame in 0..FRAMES {
        let lines = render_ascii_frame(frame, false);
        assert_eq!(lines.len(), 4);
    }
    let f0: String = render_ascii_frame(0, false)[2]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    let f1: String = render_ascii_frame(1, false)[2]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert_ne!(f0, f1);
}

/// Under ANSI-16 every sprite keeps its colour by hue — Pinky is pink, not
/// the white it collapses to by saturation — and the eyes stay white on a
/// blue pupil.
#[test]
fn ansi_sprite_colours_keep_their_hue() {
    use ratatui::style::Color;
    let body = |rgb: (u8, u8, u8)| color(rgb, false);
    assert_eq!(body(PACMAN_YELLOW), Color::LightYellow);
    assert_eq!(body(BLINKY_RED), Color::LightRed);
    assert_eq!(body(INKY_CYAN), Color::LightCyan);
    assert_eq!(body(CLYDE_ORANGE), Color::LightYellow);
    assert_eq!(body(PINKY_PINK), Color::Magenta);
    assert_eq!(body(EYE_WHITE), Color::White);
    assert_eq!(body(PUPIL_BLUE), Color::LightBlue);
    assert_eq!(body(PUPIL_BLACK), Color::Black);
}

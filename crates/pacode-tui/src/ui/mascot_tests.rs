use super::*;

#[test]
fn every_sprite_is_rectangular_and_renders_to_half_height() {
    for kind in MascotKind::ALL {
        let sprite = kind.sprite();
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

use super::*;
use pacode_render::RenderOptions;

fn info() -> HeaderInfo {
    HeaderInfo {
        version: "0.1.0".to_string(),
        mascot: mascot::MascotKind::Pacman,
        truecolor: true,
    }
}

#[test]
fn wide_header_shows_mascot_and_title_once() {
    let opts = RenderOptions::new(80, false);
    let lines = render(&info(), 80, &opts);
    let text: Vec<String> = lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    let joined = text.join("\n");
    assert_eq!(joined.matches("pacode v0.1.0").count(), 1);
    assert_eq!(lines.len(), mascot::HEIGHT + 1);
}

#[test]
fn narrow_header_drops_the_mascot() {
    let opts = RenderOptions::new(80, false);
    let lines = render(&info(), 30, &opts);
    assert_eq!(lines.len(), 2);
    let first: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(first, "pacode v0.1.0");
}

#[test]
fn header_carries_no_model_or_path_text() {
    let opts = RenderOptions::new(80, false);
    let lines = render(&info(), 100, &opts);
    let joined: String = lines
        .iter()
        .map(|l| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!joined.contains("/model"));
    assert!(!joined.contains("effort"));
    assert!(!joined.contains("Using"));
}

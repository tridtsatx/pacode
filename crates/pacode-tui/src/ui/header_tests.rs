use super::*;
use pacode_render::RenderOptions;

fn info() -> HeaderInfo {
    HeaderInfo {
        version: "0.1.0".to_string(),
        day: 0,
        mascot: mascot::MascotKind::Pacman,
        truecolor: true,
    }
}

#[test]
fn wide_header_shows_mascot_and_title_once() {
    let opts = RenderOptions::new(80, false);
    let lines = render(&info(), 80, &opts, 0);
    let text: Vec<String> = lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    let joined = text.join("\n");
    assert_eq!(joined.matches("pacode v0.1.0").count(), 1);
    assert_eq!(joined.matches("made by tridtsat").count(), 1);
    assert_eq!(lines.len(), mascot::HEIGHT + 1);
}

#[test]
fn wide_header_shows_the_phrase_of_the_day_right_aligned() {
    let opts = RenderOptions::new(120, false);
    let lines = render(&info(), 120, &opts, 0);
    let phrase = crate::ui::phrases::phrase_of_the_day(0);
    let row = lines
        .iter()
        .find(|l| {
            let t: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            t.contains("pacode v0.1.0")
        })
        .expect("title row");
    let text: String = row.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains(phrase), "phrase missing: {text}");
    assert_eq!(
        pacode_render::display_width(&text),
        120 - trailing_slack(&text)
    );
}

/// The phrase ends exactly at the right edge, so the row has no trailing padding.
fn trailing_slack(text: &str) -> usize {
    text.len() - text.trim_end().len()
}

#[test]
fn narrow_header_drops_the_phrase_but_keeps_the_byline() {
    let opts = RenderOptions::new(46, false);
    let lines = render(&info(), 46, &opts, 0);
    let joined: String = lines
        .iter()
        .map(|l| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("made by tridtsat"));
    assert!(!joined.contains(crate::ui::phrases::phrase_of_the_day(0)));
}

#[test]
fn narrow_header_drops_the_mascot() {
    let opts = RenderOptions::new(80, false);
    let lines = render(&info(), 30, &opts, 0);
    assert_eq!(lines.len(), 3);
    let first: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(first, "pacode v0.1.0");
}

#[test]
fn header_carries_no_model_or_path_text() {
    let opts = RenderOptions::new(80, false);
    let lines = render(&info(), 100, &opts, 0);
    let joined: String = lines
        .iter()
        .map(|l| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!joined.contains("/model"));
    assert!(!joined.contains("effort"));
    assert!(!joined.contains("Using"));
}

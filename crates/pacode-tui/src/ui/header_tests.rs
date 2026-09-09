use super::*;

fn make_info() -> HeaderInfo {
    HeaderInfo {
        model: "gemini-3.8-flash".into(),
        effort: "high".into(),
        provider: "bubna".into(),
        cwd: "/home/user/project".into(),
        config_path: "/home/user/.config/pacode/config.toml".into(),
        version: "0.1.0".into(),
    }
}

#[test]
fn test_header_render_unicode() {
    let info = make_info();
    let opts = RenderOptions::new(80, false);
    let lines = render(&info, 80, &opts);

    let full_text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect::<Vec<_>>()
        .join(" ");

    assert!(full_text.contains("▄▄▄▄▄"));
    assert!(full_text.contains("pacode v0.1.0"));
    assert!(full_text.contains("gemini-3.8-flash with high effort"));
    assert!(full_text.contains("bubna"));
    assert!(full_text.contains("/home/user/project"));
    assert!(
        full_text.contains("Using gemini-3.8-flash (from /home/user/.config/pacode/config.toml)")
    );
}

#[test]
fn test_header_render_ascii() {
    let info = make_info();
    let opts = RenderOptions::new(80, true);
    let lines = render(&info, 80, &opts);

    let full_text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect::<Vec<_>>()
        .join(" ");

    assert!(full_text.contains("(C"));
    assert!(!full_text.contains("▄▄▄▄▄"));
    assert!(full_text.contains("pacode v0.1.0"));
}

#[test]
fn test_header_render_narrow_width_drops_mascot() {
    let info = make_info();
    let opts = RenderOptions::new(35, false);
    let lines = render(&info, 35, &opts);

    let full_text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect::<Vec<_>>()
        .join(" ");

    assert!(!full_text.contains("▄▄▄▄▄"));
    assert!(!full_text.contains("(C"));
    assert!(full_text.contains("pacode v0.1.0"));
}

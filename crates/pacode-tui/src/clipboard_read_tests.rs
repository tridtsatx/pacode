use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use tempfile::TempDir;

use super::*;

pub struct MockCommandRunner {
    pub available: HashSet<String>,
    pub responses: HashMap<(String, Vec<String>), Result<Vec<u8>, String>>,
    pub calls: Mutex<Vec<(String, Vec<String>)>>,
}

impl MockCommandRunner {
    pub fn new() -> Self {
        Self {
            available: HashSet::new(),
            responses: HashMap::new(),
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl CommandRunner for MockCommandRunner {
    fn which(&self, binary: &str) -> bool {
        self.available.contains(binary)
    }

    fn run(&self, binary: &str, args: &[&str]) -> Result<Vec<u8>, String> {
        let key = (
            binary.to_string(),
            args.iter().map(|s| s.to_string()).collect(),
        );
        self.calls.lock().unwrap().push(key.clone());
        self.responses
            .get(&key)
            .cloned()
            .unwrap_or_else(|| Err("mock not found".to_string()))
    }
}

#[test]
fn test_detect_helper_linux_picks_wayland_over_x11() {
    let mut runner = MockCommandRunner::new();
    runner.available.insert("wl-paste".to_string());
    runner.available.insert("xclip".to_string());

    let helper = detect_clipboard_helper(&runner, ClipboardPlatform::Linux);
    assert_eq!(helper, Ok(ClipboardHelper::WlPaste));
}

#[test]
fn test_detect_helper_linux_picks_x11_when_wayland_absent() {
    let mut runner = MockCommandRunner::new();
    runner.available.insert("xclip".to_string());

    let helper = detect_clipboard_helper(&runner, ClipboardPlatform::Linux);
    assert_eq!(helper, Ok(ClipboardHelper::Xclip));
}

#[test]
fn test_detect_helper_linux_helpful_message_when_neither_present() {
    let runner = MockCommandRunner::new();

    let helper = detect_clipboard_helper(&runner, ClipboardPlatform::Linux);
    assert!(helper.is_err());
    let err = helper.unwrap_err();
    assert!(err.contains("wl-paste"));
    assert!(err.contains("xclip"));
    assert!(err.contains("Wayland") || err.contains("X11"));
}

#[test]
fn test_detect_helper_macos() {
    let mut runner = MockCommandRunner::new();
    runner.available.insert("pngpaste".to_string());

    let helper = detect_clipboard_helper(&runner, ClipboardPlatform::MacOs);
    assert_eq!(helper, Ok(ClipboardHelper::Pngpaste));

    let runner_empty = MockCommandRunner::new();
    let helper_err = detect_clipboard_helper(&runner_empty, ClipboardPlatform::MacOs);
    assert!(helper_err.is_err());
    let err = helper_err.unwrap_err();
    assert!(err.contains("pngpaste"));
    assert!(err.contains("brew install pngpaste"));
}

#[test]
fn test_read_image_data_wayland_success() {
    let mut runner = MockCommandRunner::new();
    runner.available.insert("wl-paste".to_string());
    runner.available.insert("xclip".to_string());

    let fake_png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
    runner.responses.insert(
        (
            "wl-paste".to_string(),
            vec![
                "--type".to_string(),
                "image/png".to_string(),
                "--no-newline".to_string(),
            ],
        ),
        Ok(fake_png.clone()),
    );

    let res = read_image_data(&runner, ClipboardPlatform::Linux);
    assert_eq!(res, ClipboardImageResult::Image(fake_png));

    // One type listing plus the read itself.
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|c| c.0 == "wl-paste"));
    assert_eq!(calls[0].1, vec!["--list-types".to_string()]);
}

#[test]
fn test_read_image_data_wayland_fallback_to_x11_on_failure() {
    let mut runner = MockCommandRunner::new();
    runner.available.insert("wl-paste".to_string());
    runner.available.insert("xclip".to_string());

    // wl-paste fails (e.g. not a wayland session)
    runner.responses.insert(
        (
            "wl-paste".to_string(),
            vec![
                "--type".to_string(),
                "image/png".to_string(),
                "--no-newline".to_string(),
            ],
        ),
        Err("Cannot connect to wayland".to_string()),
    );

    let fake_png = b"PNG_DATA_FROM_XCLIP".to_vec();
    runner.responses.insert(
        (
            "xclip".to_string(),
            vec![
                "-selection".to_string(),
                "clipboard".to_string(),
                "-t".to_string(),
                "image/png".to_string(),
                "-o".to_string(),
            ],
        ),
        Ok(fake_png.clone()),
    );

    let res = read_image_data(&runner, ClipboardPlatform::Linux);
    assert_eq!(res, ClipboardImageResult::Image(fake_png));

    // Type listing + failed read on wl-paste, then listing + read on xclip.
    let calls = runner.calls.lock().unwrap();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0].0, "wl-paste");
    assert_eq!(calls[1].0, "wl-paste");
    assert_eq!(calls[2].0, "xclip");
    assert_eq!(calls[3].0, "xclip");
}

#[test]
fn test_read_image_data_no_image_in_clipboard() {
    let mut runner = MockCommandRunner::new();
    runner.available.insert("wl-paste".to_string());
    runner.responses.insert(
        (
            "wl-paste".to_string(),
            vec![
                "--type".to_string(),
                "image/png".to_string(),
                "--no-newline".to_string(),
            ],
        ),
        Err("No selection".to_string()),
    );

    let res = read_image_data(&runner, ClipboardPlatform::Linux);
    assert_eq!(res, ClipboardImageResult::NoImage);
}

#[test]
fn test_read_image_data_helper_missing() {
    let runner = MockCommandRunner::new();
    let res = read_image_data(&runner, ClipboardPlatform::Linux);
    match res {
        ClipboardImageResult::HelperMissing(msg) => {
            assert!(msg.contains("wl-paste"));
            assert!(msg.contains("xclip"));
        }
        other => panic!("expected HelperMissing, got {other:?}"),
    }
}

#[test]
fn test_pasted_images_lifecycle_and_cleanup() {
    let tmp = TempDir::new().unwrap();
    let mut manager = PastedImages::new_in(tmp.path().to_path_buf());

    let image_bytes = b"fake_png_data".to_vec();
    let file_path = manager.create_image_file(&image_bytes).unwrap();

    // File exists on disk
    assert!(file_path.exists());
    assert_eq!(manager.records().len(), 1);

    // Prompt contains reference
    let prompt_with_ref = format!("Look at @{} please", file_path.display());
    manager.cleanup_unreferenced(&prompt_with_ref);
    assert!(file_path.exists());
    assert_eq!(manager.records().len(), 1);

    // Prompt no longer contains reference -> temp file is removed
    let prompt_without_ref = "Look at something else".to_string();
    manager.cleanup_unreferenced(&prompt_without_ref);
    assert!(!file_path.exists());
    assert!(manager.records().is_empty());
}

#[test]
fn test_pasted_images_submitted_are_not_deleted_by_prompt_clear() {
    let tmp = TempDir::new().unwrap();
    let mut manager = PastedImages::new_in(tmp.path().to_path_buf());

    let image_bytes = b"fake_png_data".to_vec();
    let file_path = manager.create_image_file(&image_bytes).unwrap();

    let prompt = format!("@{} ", file_path.display());
    manager.mark_submitted(&prompt);

    // Even if prompt is now empty (submitted), submitted image is preserved
    manager.cleanup_unreferenced("");
    assert!(file_path.exists());
    assert_eq!(manager.records().len(), 1);
    assert!(manager.records()[0].submitted);
}

#[test]
fn test_pasted_images_size_cap_rejected() {
    let tmp = TempDir::new().unwrap();
    let mut manager = PastedImages::new_in(tmp.path().to_path_buf());

    let large_bytes = vec![0u8; IMAGE_PASTE_MAX_BYTES + 1];
    let res = manager.create_image_file(&large_bytes);
    assert!(res.is_err());
}

fn wl_runner() -> MockCommandRunner {
    let mut runner = MockCommandRunner::new();
    runner.available.insert("wl-paste".to_string());
    runner
}

fn key(binary: &str, args: &[&str]) -> (String, Vec<String>) {
    (
        binary.to_string(),
        args.iter().map(|s| s.to_string()).collect(),
    )
}

#[test]
fn test_image_read_requests_a_jpeg_when_png_is_not_offered() {
    let mut runner = wl_runner();
    runner.responses.insert(
        key("wl-paste", &["--list-types"]),
        Ok(b"text/html\nimage/jpeg\n".to_vec()),
    );
    runner.responses.insert(
        key("wl-paste", &["--type", "image/jpeg", "--no-newline"]),
        Ok(vec![0xFF, 0xD8, 0xFF]),
    );

    let result = read_image_data(&runner, ClipboardPlatform::Linux);
    assert_eq!(result, ClipboardImageResult::Image(vec![0xFF, 0xD8, 0xFF]));
}

#[test]
fn test_image_read_prefers_png_when_several_types_are_offered() {
    let mut runner = wl_runner();
    runner.responses.insert(
        key("wl-paste", &["--list-types"]),
        Ok(b"image/webp\nimage/png\nimage/jpeg\n".to_vec()),
    );
    runner.responses.insert(
        key("wl-paste", &["--type", "image/png", "--no-newline"]),
        Ok(vec![0x89, 0x50]),
    );

    assert_eq!(
        read_image_data(&runner, ClipboardPlatform::Linux),
        ClipboardImageResult::Image(vec![0x89, 0x50])
    );
}

#[test]
fn test_image_read_falls_back_to_png_when_the_listing_fails() {
    let mut runner = wl_runner();
    runner.responses.insert(
        key("wl-paste", &["--type", "image/png", "--no-newline"]),
        Ok(vec![0x89]),
    );

    assert_eq!(
        read_image_data(&runner, ClipboardPlatform::Linux),
        ClipboardImageResult::Image(vec![0x89])
    );
}

#[test]
fn test_text_read_returns_clipboard_text() {
    let mut runner = wl_runner();
    runner.responses.insert(
        key("wl-paste", &["--no-newline", "--type", "text/plain"]),
        Ok(b"hello".to_vec()),
    );

    assert_eq!(
        read_text_data(&runner, ClipboardPlatform::Linux),
        Some("hello".to_string())
    );
}

#[test]
fn test_text_read_reports_nothing_for_empty_or_missing_clipboard() {
    let mut runner = wl_runner();
    runner.responses.insert(
        key("wl-paste", &["--no-newline", "--type", "text/plain"]),
        Ok(Vec::new()),
    );
    assert_eq!(read_text_data(&runner, ClipboardPlatform::Linux), None);

    let bare = MockCommandRunner::new();
    assert_eq!(read_text_data(&bare, ClipboardPlatform::Linux), None);
    assert_eq!(read_text_data(&bare, ClipboardPlatform::Other), None);
}

#[test]
fn test_text_read_rejects_non_utf8() {
    let mut runner = wl_runner();
    runner.responses.insert(
        key("wl-paste", &["--no-newline", "--type", "text/plain"]),
        Ok(vec![0xFF, 0xFE, 0x00]),
    );
    assert_eq!(read_text_data(&runner, ClipboardPlatform::Linux), None);
}

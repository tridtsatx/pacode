use std::fs;
use tempfile::tempdir;

use super::*;

#[test]
fn test_expand_resolves_real_file() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("hello.txt");
    fs::write(&file_path, "Hello, world!").unwrap();

    let text = "Please read @hello.txt for me";
    let expanded = expand_user_message(text, dir.path(), 16_000);

    assert!(expanded.starts_with("Please read @hello.txt for me"));
    assert!(expanded.contains("<attachment path=\"hello.txt\">"));
    assert!(expanded.contains("Hello, world!"));
    assert!(expanded.contains("</attachment>"));
}

#[test]
fn test_expand_bounds_oversized_file() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("big.txt");
    let large_content = "A".repeat(10_000);
    fs::write(&file_path, &large_content).unwrap();

    let cap = 500;
    let text = "Check @big.txt";
    let expanded = expand_user_message(text, dir.path(), cap);

    assert!(expanded.contains("<attachment path=\"big.txt\">"));
    assert!(expanded.contains("characters truncated"));
    assert!(expanded.len() < 2_000);
}

#[test]
fn test_expand_non_existent_reference_left_plain() {
    let dir = tempdir().unwrap();
    let text = "Check @does_not_exist.txt please";
    let expanded = expand_user_message(text, dir.path(), 16_000);

    assert_eq!(expanded, text);
    assert!(!expanded.contains("<attachment"));
}

#[test]
fn test_expand_escaping_email_left_plain() {
    let dir = tempdir().unwrap();
    let text = "Contact dev@example.com for info";
    let expanded = expand_user_message(text, dir.path(), 16_000);

    assert_eq!(expanded, text);
}

#[test]
fn test_expand_path_traversal_outside_cwd_left_plain() {
    let dir = tempdir().unwrap();
    let sub = dir.path().join("subdir");
    fs::create_dir_all(&sub).unwrap();
    let outside = dir.path().join("secret.txt");
    fs::write(&outside, "secret").unwrap();

    // Try to access outside file via ../
    let text = "Look at @../secret.txt";
    let expanded = expand_user_message(text, &sub, 16_000);

    assert_eq!(expanded, text);
    assert!(!expanded.contains("<attachment"));
}

#[test]
fn test_expand_binary_file_left_plain() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("binary.dat");
    fs::write(&file_path, [0u8, 1u8, 2u8, 0u8, 4u8]).unwrap();

    let text = "Check @binary.dat";
    let expanded = expand_user_message(text, dir.path(), 16_000);

    assert_eq!(expanded, text);
    assert!(!expanded.contains("<attachment"));
}

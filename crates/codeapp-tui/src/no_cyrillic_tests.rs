use std::fs;
use std::path::{Path, PathBuf};

fn collect_rs_files(dir: &Path, acc: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_rs_files(&p, acc);
            } else if p.extension().is_some_and(|ext| ext == "rs") {
                acc.push(p);
            }
        }
    }
}

fn contains_cyrillic(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, 'а'..='я' | 'А'..='Я' | 'ё' | 'Ё'))
}

#[test]
fn test_no_cyrillic_in_src_string_literals() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src_dir = manifest_dir.join("src");
    let mut files = Vec::new();
    collect_rs_files(&src_dir, &mut files);
    assert!(!files.is_empty(), "src dir should contain .rs files");

    let mut violations = Vec::new();

    for file in files {
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        for (line_no, line) in content.lines().enumerate() {
            // Heuristic: line contains `"` and a Cyrillic char
            if line.contains('"') && contains_cyrillic(line) {
                violations.push(format!(
                    "{}:{}: {}",
                    file.display(),
                    line_no + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Found Cyrillic letters inside string literal in src/:\n{}",
        violations.join("\n")
    );
}

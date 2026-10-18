use std::fs;

use crate::md5::md5_hex;
use crate::test_util::{ok, run_in, sh};

#[test]
fn md5_known_vectors() {
    assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(
        md5_hex(b"The quick brown fox jumps over the lazy dog"),
        "9e107d9d372bb6826bd81d3542a419d6"
    );
    assert_eq!(
        md5_hex(&vec![b'a'; 1_000_000]),
        "7707d6ae4e027c70eea2a935c2296f21"
    );
}

#[test]
fn sha256sum_and_md5sum_format() {
    assert_eq!(
        ok("sha256sum", &[], ""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  -\n"
    );
    assert_eq!(
        ok("sha256sum", &[], "abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  -\n"
    );
    assert_eq!(
        ok("md5sum", &[], "abc"),
        "900150983cd24fb0d6963f7d28e17f72  -\n"
    );
}

#[test]
fn checksum_check_mode() {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("f"), "hello").expect("write");
    let line = run_in(dir.path(), "sha256sum", &["f"], None).stdout_lossy();
    let good = run_in(dir.path(), "sha256sum", &["-c"], Some(line.as_str()));
    assert_eq!((good.code, good.stdout_lossy()), (0, "f: OK\n".to_string()));

    fs::write(dir.path().join("f"), "changed").expect("write");
    let bad = run_in(dir.path(), "sha256sum", &["-c"], Some(line.as_str()));
    assert_eq!(bad.code, 1);
    assert_eq!(bad.stdout_lossy(), "f: FAILED\n");
    assert!(bad.stderr.contains("did NOT match"), "{}", bad.stderr);
}

#[test]
fn base64_encode_decode() {
    assert_eq!(ok("base64", &[], "hello"), "aGVsbG8=\n");
    assert_eq!(ok("base64", &["-w", "0"], "hello"), "aGVsbG8=");
    assert_eq!(ok("base64", &["-d"], "aGVsbG8=\n"), "hello");
    assert_eq!(ok("base64", &[], ""), "");
    let encoded = ok("base64", &[], "héllo wörld");
    assert_eq!(ok("base64", &["-d"], &encoded), "héllo wörld");
}

#[test]
fn base64_wraps_at_76_columns() {
    let out = ok("base64", &[], &"a".repeat(100));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].len(), 76);
}

#[test]
fn base64_rejects_invalid_input() {
    assert_eq!(sh("base64", &["-d"], Some("!!!")).1, 1);
}

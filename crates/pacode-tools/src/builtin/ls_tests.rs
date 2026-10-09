use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::test_support::{StubToolHost, stub_ctx};

/// The walk no longer carries sizes; ls stats its own entries and still
/// prints directories first, then files with their byte size.
#[tokio::test]
async fn files_show_their_size_and_dirs_come_first() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("b.txt"), "12345").expect("write");
    std::fs::write(tmp.path().join("a.txt"), "").expect("write");
    std::fs::create_dir(tmp.path().join("zdir")).expect("mkdir");
    let mut ctx = stub_ctx(Arc::new(StubToolHost::default()));
    ctx.cwd = tmp.path().to_path_buf();

    let out = LsTool::default().call(json!({}), &ctx).await.expect("ls");
    assert_eq!(out.content, "zdir/\na.txt (0 B)\nb.txt (5 B)");
}

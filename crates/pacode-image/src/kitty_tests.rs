use super::*;

#[test]
fn test_kitty_empty_payload() {
    let chunks = chunk_kitty_base64("", 10, 20, 5, 10);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0], "\x1b_Ga=T,f=32,s=10,v=20,c=5,r=10,m=0;\x1b\\");
}

#[test]
fn test_kitty_single_chunk_under_4096() {
    // 300 bytes of base64 fits in 1 chunk
    let b64 = "A".repeat(300);
    let chunks = chunk_kitty_base64(&b64, 50, 60, 25, 30);
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].starts_with("\x1b_Ga=T,f=32,s=50,v=60,c=25,r=30,m=0;"));
    assert!(chunks[0].ends_with("\x1b\\"));
    assert!(chunks[0].contains(&b64));
}

#[test]
fn test_kitty_exactly_4096_bytes() {
    let b64 = "B".repeat(4096);
    let chunks = chunk_kitty_base64(&b64, 100, 100, 40, 20);
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].starts_with("\x1b_Ga=T,f=32,s=100,v=100,c=40,r=20,m=0;"));
    assert!(chunks[0].ends_with("\x1b\\"));
}

#[test]
fn test_kitty_multi_chunk_m_flags() {
    // 4096 * 2 + 100 = 8292 base64 characters -> produces exactly 3 chunks
    let b64 = "C".repeat(4096 * 2 + 100);
    let chunks = chunk_kitty_base64(&b64, 200, 150, 80, 40);
    assert_eq!(chunks.len(), 3);

    // Chunk 0: full metadata, m=1
    assert!(
        chunks[0].starts_with("\x1b_Ga=T,f=32,s=200,v=150,c=80,r=40,m=1;"),
        "chunk 0 must have m=1"
    );
    assert!(chunks[0].ends_with("\x1b\\"));

    // Chunk 1: intermediate, m=1
    assert!(chunks[1].starts_with("\x1b_Gm=1;"), "chunk 1 must have m=1");
    assert!(chunks[1].ends_with("\x1b\\"));

    // Chunk 2: final, m=0
    assert!(chunks[2].starts_with("\x1b_Gm=0;"), "chunk 2 must have m=0");
    assert!(chunks[2].ends_with("\x1b\\"));
}

#[test]
fn test_kitty_known_rgba_buffer_chunking() {
    // 4 bytes per pixel. A 64x64 RGBA image has 64 * 64 * 4 = 16,384 bytes.
    // 16,384 raw bytes base64-encoded: ceil(16384 / 3) * 4 = 21,848 base64 bytes.
    // 21,848 / 4096 = 5 full chunks of 4096 + 1 tail chunk of 1368 = 6 chunks.
    let rgba = vec![128u8; 64 * 64 * 4];
    let b64 = base64::engine::general_purpose::STANDARD.encode(&rgba);
    let chunks = chunk_kitty_base64(&b64, 64, 64, 32, 16);

    assert_eq!(chunks.len(), 6);
    assert!(chunks[0].starts_with("\x1b_Ga=T,f=32,s=64,v=64,c=32,r=16,m=1;"));
    for chunk in &chunks[1..5] {
        assert!(chunk.starts_with("\x1b_Gm=1;"));
        assert!(chunk.ends_with("\x1b\\"));
    }
    assert!(chunks[5].starts_with("\x1b_Gm=0;"));
    assert!(chunks[5].ends_with("\x1b\\"));

    let full_escape = encode_kitty(&rgba, 64, 64, 32, 16);
    assert_eq!(full_escape, chunks.concat());
}

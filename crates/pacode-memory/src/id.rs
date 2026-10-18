//! ULID-like ids: 10 Crockford-base32 chars of milliseconds (sortable) + 8 random chars.
//! No dependencies: randomness comes from std's per-process random hasher keys.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn new_id(now_ms: i64) -> String {
    let mut out = String::with_capacity(18);
    let ms = now_ms.max(0) as u64;
    for i in (0..10).rev() {
        out.push(ALPHABET[((ms >> (i * 5)) & 31) as usize] as char);
    }
    let mut h = RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u64(ms);
    h.write_u32(std::process::id());
    let r = h.finish();
    for i in 0..8 {
        out.push(ALPHABET[((r >> (i * 5)) & 31) as usize] as char);
    }
    out
}

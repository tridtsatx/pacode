//! Optional semantic layer: trait, vector math, blob encoding and the hybrid scoring
//! function. OFF by default (`[memory] embeddings = false`) and no embedder
//! implementation ships here.
//!
//! Why no external vector database: pacode is a single binary, a person's memory is a
//! few hundred short notes per scope, and FTS5 (BM25 + prefix) already finds most hits.
//! A brute-force cosine scan over at most [`MAX_VECTOR_SCAN`] vectors (5000 x 768 f32
//! ~ 15 MB, a few ms) is simpler and cheaper than running or linking a vector index,
//! and keeps everything in the one SQLite file next to the entries.
//!
//! Hybrid recall: `score = bm25_norm * 0.6 + cosine * 0.4`, where `bm25_norm` is the
//! relevance divided by the best relevance among the candidates (0..=1) and `cosine`
//! is clamped to 0..=1. Entry boosts (recency, pin, recall count) multiply afterwards.

use crate::error::MemoryError;

/// Cap on vectors scanned per scope.
pub const MAX_VECTOR_SCAN: usize = 5000;
pub const BM25_WEIGHT: f64 = 0.6;
pub const COSINE_WEIGHT: f64 = 0.4;

/// Produces embeddings for texts. Implementations must be cheap to share.
pub trait Embedder: Send + Sync {
    /// One vector per input text, each of length [`Embedder::dim`].
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MemoryError>;
    fn dim(&self) -> usize;
    /// Stable id (model name); vectors from different ids are never mixed.
    fn id(&self) -> &str;
}

pub fn hybrid_score(bm25_norm: f64, cosine: f64) -> f64 {
    bm25_norm.clamp(0.0, 1.0) * BM25_WEIGHT + cosine.clamp(0.0, 1.0) * COSINE_WEIGHT
}

/// Cosine similarity; 0.0 for mismatched or zero vectors.
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (f64::from(*x), f64::from(*y));
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}

/// f32 little-endian blob.
pub fn vector_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn blob_to_vector(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

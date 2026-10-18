use std::sync::Arc;

use crate::embed::{
    BM25_WEIGHT, COSINE_WEIGHT, blob_to_vector, cosine, hybrid_score, vector_to_blob,
};
use crate::text::token_list;
use crate::*;

/// Axis 0: vehicles, axis 1: desserts, axis 2: everything else.
struct FakeEmbedder;

impl Embedder for FakeEmbedder {
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, MemoryError> {
        Ok(texts
            .iter()
            .map(|t| {
                let mut v = vec![0.0f32; 3];
                for tok in token_list(t) {
                    match tok.as_str() {
                        "car" | "automobile" | "vehicle" => v[0] += 1.0,
                        "cake" | "pie" | "dessert" => v[1] += 1.0,
                        _ => v[2] += 0.01,
                    }
                }
                v
            })
            .collect())
    }
    fn dim(&self) -> usize {
        3
    }
    fn id(&self) -> &str {
        "fake-3"
    }
}

#[test]
fn vector_math_and_blob_round_trip() {
    assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-9);
    assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-9);
    assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
    assert_eq!(cosine(&[0.0, 0.0], &[1.0, 2.0]), 0.0);
    let v = vec![0.5f32, -1.25, 3.0];
    let blob = vector_to_blob(&v);
    assert_eq!(blob.len(), 12);
    assert_eq!(blob[..4], 0.5f32.to_le_bytes());
    assert_eq!(blob_to_vector(&blob), v);
}

#[test]
fn hybrid_score_weights() {
    assert!((BM25_WEIGHT + COSINE_WEIGHT - 1.0).abs() < 1e-12);
    assert!((hybrid_score(1.0, 0.0) - 0.6).abs() < 1e-12);
    assert!((hybrid_score(0.0, 1.0) - 0.4).abs() < 1e-12);
    assert!((hybrid_score(1.0, 1.0) - 1.0).abs() < 1e-12);
    assert!(
        (hybrid_score(0.5, -0.5) - 0.3).abs() < 1e-12,
        "negative cosine clamps to 0"
    );
}

#[test]
fn semantic_layer_finds_synonyms_only_when_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let store = MemoryStore::for_dirs(&dir.path().join("cfg"), &dir.path().join("proj"));
    store.set_embedder(Some(Arc::new(FakeEmbedder)));
    store
        .write(
            Scope::Global,
            NewEntry::new(Kind::Fact, "Vehicle note", "my car is red"),
        )
        .unwrap();
    store
        .write(
            Scope::Global,
            NewEntry::new(Kind::Fact, "Dessert", "cake and pie recipe"),
        )
        .unwrap();

    // "automobile" shares no token with any entry: only the vector scan can find it.
    let hits = store.recall(&RecallRequest::new("automobile")).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].entry.title, "Vehicle note");

    // Lexical + semantic candidates merge; the unrelated dessert stays out.
    store
        .write(
            Scope::Global,
            NewEntry::new(Kind::Fact, "Red things", "a red balloon and a red car park"),
        )
        .unwrap();
    let hits = store.recall(&RecallRequest::new("red car")).unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|h| h.entry.title != "Dessert"));

    // Disabled: no semantic recall.
    store.set_embedder(None);
    assert!(
        store
            .recall(&RecallRequest::new("automobile"))
            .unwrap()
            .is_empty()
    );
}

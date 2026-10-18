//! Per-scope candidate gathering and scoring for recall, lexical or hybrid.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, params};

use crate::embed::{MAX_VECTOR_SCAN, blob_to_vector, cosine, hybrid_score, vector_to_blob};
use crate::error::MemoryError;
use crate::ops::{self, LIVE};
use crate::rank::boost;
use crate::types::{Entry, Hit, Kind, Scope};

/// FTS candidates fetched per scope before ranking.
const FTS_CANDIDATES: usize = 100;
/// Semantic-only candidates (no lexical match) added per scope.
const SEMANTIC_EXTRA: usize = 20;
const SEMANTIC_MIN_COSINE: f64 = 0.3;

/// Query vector plus the id of the embedder that produced it.
pub(crate) struct QueryVec<'a> {
    pub vector: &'a [f32],
    pub embedder_id: &'a str,
}

pub(crate) fn upsert_embedding(
    conn: &Connection,
    entry_id: &str,
    embedder_id: &str,
    vector: &[f32],
) -> Result<(), MemoryError> {
    conn.execute(
        "INSERT INTO memory_embeddings (entry_id, embedder_id, dim, vector) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(entry_id, embedder_id) DO UPDATE SET dim = ?3, vector = ?4",
        params![
            entry_id,
            embedder_id,
            vector.len() as i64,
            vector_to_blob(vector)
        ],
    )?;
    Ok(())
}

/// Vectors of live entries for one embedder, most recently updated first, capped.
fn scan_vectors(
    conn: &Connection,
    embedder_id: &str,
    dim: usize,
) -> Result<Vec<(String, Vec<f32>)>, MemoryError> {
    let sql = format!(
        "SELECT m.entry_id, m.vector FROM memory_embeddings m \
         JOIN memory_entries e ON e.id = m.entry_id \
         WHERE m.embedder_id = ?1 AND m.dim = ?2 AND {LIVE} \
         ORDER BY e.updated_ms DESC LIMIT ?3"
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(
        params![embedder_id, dim as i64, MAX_VECTOR_SCAN as i64],
        |r| {
            let blob: Vec<u8> = r.get(1)?;
            Ok((r.get::<_, String>(0)?, blob_to_vector(&blob)))
        },
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn get_many(
    conn: &Connection,
    scope: Scope,
    ids: &[&str],
    kind: Option<Kind>,
) -> Result<Vec<Entry>, MemoryError> {
    let mut out = Vec::new();
    for id in ids {
        if let Some(e) = ops::get_live(conn, scope, id)?
            && kind.is_none_or(|k| k == e.kind)
        {
            out.push(e);
        }
    }
    Ok(out)
}

/// Rank one scope for a query. Lexical only when `qv` is `None`; otherwise hybrid.
pub(crate) fn rank_scope(
    conn: &Connection,
    scope: Scope,
    fts: &str,
    kind: Option<Kind>,
    qv: Option<QueryVec<'_>>,
    now_ms: i64,
) -> Result<Vec<Hit>, MemoryError> {
    let lexical = ops::search(conn, scope, fts, kind, FTS_CANDIDATES)?;
    let Some(qv) = qv else {
        return Ok(lexical
            .into_iter()
            .map(|(e, rel)| {
                let score = rel * boost(&e, now_ms);
                Hit { entry: e, score }
            })
            .collect());
    };

    let vectors = scan_vectors(conn, qv.embedder_id, qv.vector.len())?;
    let cos: HashMap<&str, f64> = vectors
        .iter()
        .map(|(id, v)| (id.as_str(), cosine(qv.vector, v)))
        .collect();
    let max_rel = lexical.iter().map(|(_, r)| *r).fold(0.0f64, f64::max);

    let mut hits: Vec<Hit> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (e, rel) in &lexical {
        let norm = if max_rel > 0.0 { rel / max_rel } else { 0.0 };
        let c = cos.get(e.id.as_str()).copied().unwrap_or(0.0);
        hits.push(Hit {
            entry: e.clone(),
            score: hybrid_score(norm, c) * boost(e, now_ms),
        });
        seen.insert(e.id.as_str());
    }
    let mut extra: Vec<(&str, f64)> = Vec::new();
    for (id, c) in &cos {
        if *c >= SEMANTIC_MIN_COSINE && !seen.contains(id) {
            extra.push((*id, *c));
        }
    }
    extra.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    extra.truncate(SEMANTIC_EXTRA);
    let ids: Vec<&str> = extra.iter().map(|(id, _)| *id).collect();
    for e in get_many(conn, scope, &ids, kind)? {
        let c = cos.get(e.id.as_str()).copied().unwrap_or(0.0);
        hits.push(Hit {
            score: hybrid_score(0.0, c) * boost(&e, now_ms),
            entry: e,
        });
    }
    Ok(hits)
}

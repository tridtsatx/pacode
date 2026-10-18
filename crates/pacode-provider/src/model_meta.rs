//! Catalog metadata for the model picker: reasoning levels, kind, price, output
//! limit. Read from a provider's `/models` entry where it states them, otherwise
//! derived from the endpoint's reasoning convention ([`ReasoningWire`]) and a few
//! well-known id patterns. Unknown stays unknown: empty / `None`, never a guess.

use pacode_types::{Effort, ModelInfo, ModelKind, Pricing};
use serde_json::Value;

use crate::compat::ReasoningWire;

#[cfg(test)]
#[path = "model_meta_tests.rs"]
mod model_meta_tests;

/// Reasoning levels `model` accepts on `wire`, weakest first. Callers pass a model
/// already known to reason; the levels follow what [`crate::Compat::effort_str`]
/// can put on the wire for that endpoint.
pub fn efforts_for(wire: ReasoningWire, model: &str) -> Vec<Effort> {
    let lower = model.to_ascii_lowercase();
    match wire {
        // No effort control: nothing to choose (Z.AI / Kimi K2.x only toggle thinking).
        ReasoningWire::Unsupported | ReasoningWire::ThinkingToggle => Vec::new(),
        ReasoningWire::ReasoningEffortTiered => vec![Effort::Low, Effort::High, Effort::Max],
        ReasoningWire::ReasoningObject => vec![Effort::Low, Effort::Medium, Effort::High],
        ReasoningWire::ReasoningEffort => {
            if lower.contains("gpt-5") && lower.contains("-pro") {
                vec![Effort::High]
            } else if lower.contains("gemini-3") && lower.contains("pro") {
                vec![Effort::Low, Effort::High]
            } else {
                vec![Effort::Low, Effort::Medium, Effort::High]
            }
        }
    }
}

/// Kind guessed from the model id alone.
pub fn kind_from_id(model: &str) -> ModelKind {
    let lower = model.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));
    if has(&["embed"]) {
        ModelKind::Embedding
    } else if has(&[
        "dall-e",
        "gpt-image",
        "imagen",
        "stable-diffusion",
        "flux",
        "image-preview",
        "-image",
    ]) && !has(&["vision"])
    {
        ModelKind::Image
    } else if has(&["whisper", "tts", "moderation", "rerank", "transcribe"]) {
        ModelKind::Other
    } else {
        ModelKind::Chat
    }
}

/// USD per token as the catalogs print it (`"0.0000005"` or a bare number) →
/// USD per million tokens. Negative or unparsable values (OpenRouter prints `-1`
/// for routers with no fixed price) are unknown.
fn per_million(v: &Value) -> Option<f64> {
    let per_token = match v {
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    (per_token.is_finite() && per_token >= 0.0).then(|| round6(per_token * 1e6))
}

fn round6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

fn u32_of(v: &Value) -> Option<u32> {
    let n = v.as_u64().or_else(|| v.as_str()?.trim().parse().ok())?;
    u32::try_from(n).ok().filter(|n| *n > 0)
}

fn first_u32(item: &Value, paths: &[&[&str]]) -> Option<u32> {
    paths.iter().find_map(|path| {
        let mut cur = item;
        for key in *path {
            cur = cur.get(*key)?;
        }
        u32_of(cur)
    })
}

fn modalities(arch: &Value, key: &str) -> Option<Vec<String>> {
    let arr = arch.get(key)?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|m| m.as_str().map(str::to_ascii_lowercase))
            .collect(),
    )
}

/// Kind declared by a catalog entry's `architecture`, when it declares one.
fn kind_from_catalog(item: &Value) -> Option<ModelKind> {
    let arch = item.get("architecture")?;
    if let Some(out) = modalities(arch, "output_modalities") {
        if out.iter().any(|m| m.contains("embed")) {
            return Some(ModelKind::Embedding);
        }
        if out.iter().any(|m| m == "text") {
            return Some(ModelKind::Chat);
        }
        if out.iter().any(|m| m == "image") {
            return Some(ModelKind::Image);
        }
        if !out.is_empty() {
            return Some(ModelKind::Other);
        }
    }
    let modality = arch.get("modality").and_then(Value::as_str)?;
    let output = modality.rsplit("->").next().unwrap_or(modality);
    if output.contains("embed") {
        Some(ModelKind::Embedding)
    } else if output.contains("text") {
        Some(ModelKind::Chat)
    } else if output.contains("image") {
        Some(ModelKind::Image)
    } else {
        None
    }
}

/// Fold one `/models` entry (OpenRouter shape, partly shared by other gateways)
/// into `info`. Values the entry does not state are left as they were.
///
/// `trust_reasoning` lets the entry's `supported_parameters` decide the reasoning
/// flag; pass `false` when the user's config pinned it. The effort list is
/// recomputed from `wire` afterwards.
pub fn apply_catalog_item(
    info: &mut ModelInfo,
    item: &Value,
    wire: ReasoningWire,
    trust_reasoning: bool,
) {
    // A window the user configured wins over the catalog's.
    if info.context_window.is_none()
        && let Some(ctx) = first_u32(
            item,
            &[
                &["context_length"],
                &["context_window"],
                &["max_context_length"],
            ],
        )
    {
        info.context_window = Some(ctx);
    }
    if let Some(out) = first_u32(
        item,
        &[
            &["top_provider", "max_completion_tokens"],
            &["max_completion_tokens"],
            &["max_output_tokens"],
        ],
    ) {
        info.max_output = Some(out);
    }
    if info.pricing.is_none()
        && let Some(p) = item.get("pricing")
        && let (Some(input), Some(output)) = (
            p.get("prompt").and_then(per_million),
            p.get("completion").and_then(per_million),
        )
    {
        info.pricing = Some(Pricing {
            input_per_m: input,
            output_per_m: output,
            cache_read_per_m: p.get("input_cache_read").and_then(per_million),
            cache_write_per_m: p.get("input_cache_write").and_then(per_million),
        });
    }
    if let Some(kind) = kind_from_catalog(item) {
        info.kind = kind;
    }
    if trust_reasoning
        && let Some(params) = item.get("supported_parameters").and_then(Value::as_array)
    {
        info.supports_reasoning = params
            .iter()
            .filter_map(Value::as_str)
            .any(|p| p == "reasoning" || p == "include_reasoning");
    }
    finish(info, wire);
}

/// Derive the fields that follow from the others: reasoning levels from the wire,
/// and no reasoning for models that are not chat models.
pub fn finish(info: &mut ModelInfo, wire: ReasoningWire) {
    if info.kind != ModelKind::Chat {
        info.supports_reasoning = false;
    }
    info.efforts = if info.supports_reasoning {
        efforts_for(wire, &info.route.model)
    } else {
        Vec::new()
    };
}

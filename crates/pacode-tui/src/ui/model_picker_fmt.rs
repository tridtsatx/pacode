//! Number and text formats of the model picker: context windows, prices and the
//! one-line detail of a model.

use pacode_types::{ModelInfo, Pricing};

/// `1_048_576` → `1m`, `131_072` → `131k`.
pub fn fmt_ctx(n: u32) -> String {
    if n >= 1_000_000 {
        let m = (f64::from(n) / 100_000.0).round() / 10.0;
        if (m - m.round()).abs() < f64::EPSILON {
            format!("{}m", m as u32)
        } else {
            format!("{m:.1}m")
        }
    } else if n >= 1000 {
        format!("{}k", (n + 500) / 1000)
    } else {
        n.to_string()
    }
}

fn fmt_money(x: f64) -> String {
    let s = format!("{x:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// `free` or `$0.5/1.5` (USD per million tokens in / out).
pub fn fmt_price(p: &Pricing) -> String {
    if p.input_per_m == 0.0 && p.output_per_m == 0.0 {
        "free".to_string()
    } else {
        format!(
            "${}/{}",
            fmt_money(p.input_per_m),
            fmt_money(p.output_per_m)
        )
    }
}

/// `name · 1m ctx · 66k out · $3/$15 per M · reasoning low/high · vision`;
/// parts the catalog does not state are left out.
pub fn detail_text(m: &ModelInfo, with_levels: bool) -> String {
    let mut parts = vec![m.display_name.clone()];
    if let Some(c) = m.context_window {
        parts.push(format!("{} ctx", fmt_ctx(c)));
    }
    if let Some(o) = m.max_output {
        parts.push(format!("{} out", fmt_ctx(o)));
    }
    if let Some(p) = &m.pricing {
        parts.push(match fmt_price(p) {
            f if f == "free" => f,
            f => format!("{f} per M"),
        });
    }
    if m.supports_reasoning {
        if m.efforts.is_empty() || !with_levels {
            parts.push("reasoning".to_string());
        } else {
            let levels: Vec<&str> = m.efforts.iter().map(|e| e.as_str()).collect();
            parts.push(format!("reasoning {}", levels.join("/")));
        }
    }
    if m.vision {
        parts.push("vision".to_string());
    }
    parts.join(" · ")
}

//! 1b: model price table → per-run cost estimation.
//!
//! Pure computation (no I/O): given the app config's `model_prices` table
//! (USD per 1M tokens, prompt and completion billed separately) and a run's
//! model + token counts, produce the run's estimated cost in USD.
//!
//! Conventions:
//! - Case-insensitive EXACT model-name match (no prefix/fuzzy matching —
//!   model strings are server-specific identifiers).
//! - Unknown model or empty table → `0.0` ("recorded but unpriced", not
//!   "free"). The Run/Eval line still carries the model name, so the data
//!   stays re-costable in principle.

use crate::config::ModelPrice;

/// Estimated cost in USD of one LLM call (or a run's token totals).
///
/// Returns `0.0` when `model` is unknown to `prices` (or `prices` is empty)
/// — "recorded but unpriced".
pub fn cost_usd(
    prices: &[ModelPrice],
    model: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
) -> f64 {
    let Some(p) = prices.iter().find(|p| p.model.eq_ignore_ascii_case(model)) else {
        return 0.0;
    };
    (prompt_tokens as f64 * p.per_1_m_in_usd + completion_tokens as f64 * p.per_1_m_out_usd)
        / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price(model: &str, in_usd: f64, out_usd: f64) -> ModelPrice {
        ModelPrice {
            model: model.to_string(),
            per_1_m_in_usd: in_usd,
            per_1_m_out_usd: out_usd,
        }
    }

    #[test]
    fn unknown_model_and_empty_table_are_unpriced() {
        let prices = vec![price("alpha", 2.0, 8.0)];
        assert_eq!(cost_usd(&[], "alpha", 100, 100), 0.0);
        assert_eq!(cost_usd(&prices, "beta", 100, 100), 0.0);
        assert_eq!(cost_usd(&prices, "", 100, 100), 0.0);
    }

    #[test]
    fn match_is_case_insensitive_exact() {
        let prices = vec![price("GPT-4o-mini", 0.15, 0.60)];
        let cost = cost_usd(&prices, "gpt-4O-mini", 1_000_000, 500_000);
        assert!((cost - (0.15 + 0.60 * 0.5)).abs() < 1e-12, "got: {cost}");
    }

    #[test]
    fn arithmetic_per_million() {
        let prices = vec![price("m", 2.0, 4.0)];
        // 1M in + 1M out → 2 + 4 = 6 USD.
        let c = cost_usd(&prices, "m", 1_000_000, 1_000_000);
        assert!((c - 6.0).abs() < 1e-9, "got: {c}");
        assert_eq!(cost_usd(&prices, "m", 0, 0), 0.0);
    }
}

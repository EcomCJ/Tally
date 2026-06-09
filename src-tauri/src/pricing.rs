// Per-model pricing in USD per 1M tokens.
// Source: https://platform.claude.com/docs/en/about-claude/pricing (verified 2026-05-25).
// Fable 5 source: https://www.anthropic.com/claude/fable (verified 2026-06-09).
// Cache-read is 10% of base input; cache-write (5-min ephemeral, the default)
// is 125% of base input. Tally doesn't distinguish 5m vs 1h cache writes since
// JSONL doesn't carry that bit — 5m is by far the more common default.
//
// Anthropic dropped Opus pricing dramatically with Opus 4.5: the 4.5/4.6/4.7/4.8
// generation runs at $5/$25 vs the deprecated Opus 4/4.1 at $15/$75. Haiku
// pricing went UP slightly with 4.5 ($1/$5) vs retired 3.5 ($0.80/$4).

#[derive(Debug, Clone, Copy)]
pub struct ModelRate {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

const fn rate(input: f64, output: f64) -> ModelRate {
    ModelRate {
        input,
        output,
        cache_read: input * 0.10,
        cache_write: input * 1.25,
    }
}

/// Look up Claude pricing by model name. Version-aware so the post-Opus-4.5
/// price drop is reflected and Haiku generations are billed correctly.
///
/// Recognized SKU patterns (case-insensitive, substring match):
/// - `claude-fable-5-*`, `claude-fable-5`, or observed `mythos` labels
///   -> Fable/Mythos frontier rate ($10/$50)
/// - `claude-opus-4-5-*` through `opus-4-9-*` and `opus-5-*` → new Opus
/// - `claude-opus-4-1-*`, `opus-4-DATE`, bare `opus`, `claude-3-opus`
///   → legacy Opus (deprecated $15/$75)
/// - `claude-haiku-4-5-*` and forward → new Haiku
/// - `claude-3-5-haiku-*` or bare `haiku` → retired Haiku 3.5
/// - any `sonnet` (4 / 4.5 / 4.6) → unchanged $3/$15
/// - unknown → Sonnet rate (safest middle estimate)
pub fn claude_rate(model: &str) -> ModelRate {
    let m = model.to_lowercase();
    if m.contains("fable") || m.contains("mythos") {
        return rate(10.00, 50.00);
    }
    if m.contains("opus") {
        if has_modern_minor(&m, "opus") {
            // Opus 4.5 / 4.6 / 4.7 / 4.8 + future ≥4.5
            return rate(5.00, 25.00);
        }
        // Opus 4 (deprecated), Opus 4.1 (deprecated), Claude 3 Opus — legacy
        return rate(15.00, 75.00);
    }
    if m.contains("haiku") {
        if has_modern_minor(&m, "haiku") {
            // Haiku 4.5 + future ≥4.5
            return rate(1.00, 5.00);
        }
        // Haiku 3.5 (retired except Bedrock/Vertex), older Haiku
        return rate(0.80, 4.00);
    }
    if m.contains("sonnet") {
        // Sonnet 4 (deprecated), 4.5, 4.6 — all share standard Sonnet pricing
        return rate(3.00, 15.00);
    }
    // Unknown — default to Sonnet (middle of the range, won't wildly over-
    // or under-report on a new model we haven't seen yet).
    rate(3.00, 15.00)
}

/// Detects whether the model id contains a `<family>-X-Y` segment where the
/// generation has hit Anthropic's "modern" pricing threshold (X=4 AND Y≥5,
/// or X≥5). Supports both dash (`4-5`) and dot (`4.5`) separators.
///
/// Examples for family=`opus`:
/// - `claude-opus-4-5-20250929`  → true  (Opus 4.5)
/// - `claude-opus-4-7-20260301`  → true  (Opus 4.7)
/// - `claude-opus-5-0-20270101`  → true  (future Opus 5.x)
/// - `claude-opus-4-1-20250805`  → false (Opus 4.1, legacy)
/// - `claude-opus-4-20250514`    → false (Opus 4, legacy)
/// - `claude-3-opus-20240229`    → false (Opus 3, legacy)
fn has_modern_minor(m: &str, family: &str) -> bool {
    // X≥5 (next major generation — opus-5, haiku-5, ...)
    for sep in ['-', '.'] {
        let needle = format!("{family}{sep}5");
        if m.contains(&needle) {
            return true;
        }
    }
    // 4.5 through 4.9 (current generation modern pricing)
    for minor in 5..=9 {
        for sep in ['-', '.'] {
            let needle = format!("{family}-4{sep}{minor}");
            if m.contains(&needle) {
                return true;
            }
            let needle2 = format!("{family}.4{sep}{minor}");
            if m.contains(&needle2) {
                return true;
            }
        }
    }
    false
}

#[derive(Debug, Clone, Copy)]
pub struct CodexRate {
    pub input: f64,
    pub cached_input: f64,
    pub output: f64, // reasoning charged at this rate too
}

/// Look up Codex/OpenAI pricing by model name.
/// Source: https://platform.openai.com/docs/pricing (verified 2026-05-30).
/// Also supports internal Codex labels observed in local JSONL, such as
/// `gpt-5.4` and `gpt-5.5`, so historical rows do not fall through.
pub fn codex_rate(model: &str) -> CodexRate {
    let m = model.to_lowercase();

    if m.contains("gpt-5.2-pro") || m.contains("gpt-5-2-pro") {
        return CodexRate {
            input: 21.00,
            cached_input: 21.00,
            output: 168.00,
        };
    }
    if m.contains("gpt-5-pro") {
        return CodexRate {
            input: 15.00,
            cached_input: 15.00,
            output: 120.00,
        };
    }
    if m.contains("gpt-5.2-codex")
        || m.contains("gpt-5-2-codex")
        || m.contains("gpt-5.2-chat-latest")
        || m.contains("gpt-5-2-chat-latest")
        || m == "gpt-5.2"
        || m.starts_with("gpt-5.2-")
        || m == "gpt-5-2"
    {
        return CodexRate {
            input: 1.75,
            cached_input: 0.175,
            output: 14.00,
        };
    }
    if m.contains("gpt-5.3-codex")
        || m.contains("gpt-5-3-codex")
        || m.contains("gpt-5.3-chat")
        || m.contains("gpt-5-3-chat")
    {
        return CodexRate {
            input: 1.75,
            cached_input: 0.175,
            output: 14.00,
        };
    }
    if m.contains("gpt-5-mini") {
        return CodexRate {
            input: 0.25,
            cached_input: 0.025,
            output: 2.00,
        };
    }
    if m.contains("gpt-5-nano") {
        return CodexRate {
            input: 0.05,
            cached_input: 0.005,
            output: 0.40,
        };
    }
    if m.contains("gpt-5.1-codex")
        || m.contains("gpt-5-1-codex")
        || m.contains("gpt-5.1-chat-latest")
        || m.contains("gpt-5-1-chat-latest")
        || m == "gpt-5.1"
        || m.starts_with("gpt-5.1-")
        || m == "gpt-5-1"
    {
        return CodexRate {
            input: 1.25,
            cached_input: 0.125,
            output: 10.00,
        };
    }
    if m.contains("gpt-5-codex") || m.contains("gpt-5-chat-latest") || m == "gpt-5" {
        return CodexRate {
            input: 1.25,
            cached_input: 0.125,
            output: 10.00,
        };
    }

    // Internal Codex/ChatGPT labels observed in local JSONL. Preserve explicit
    // rates so historical rows do not fall through when public SKU names move.
    if m.contains("gpt-5.4-mini") || m.contains("gpt-5-4-mini") {
        return CodexRate {
            input: 0.75,
            cached_input: 0.075,
            output: 4.50,
        };
    }
    if m.contains("gpt-5.4-nano") || m.contains("gpt-5-4-nano") {
        return CodexRate {
            input: 0.20,
            cached_input: 0.02,
            output: 1.25,
        };
    }
    if m.contains("gpt-5.4-pro") || m.contains("gpt-5-4-pro") {
        return CodexRate {
            input: 30.00,
            cached_input: 30.00,
            output: 180.00,
        };
    }
    if m.contains("gpt-5.5-pro") || m.contains("gpt-5-5-pro") {
        return CodexRate {
            input: 30.00,
            cached_input: 30.00,
            output: 180.00,
        };
    }
    if m.contains("gpt-5.4") || m.contains("gpt-5-4") {
        return CodexRate {
            input: 2.50,
            cached_input: 0.25,
            output: 15.00,
        };
    }
    if m.contains("gpt-5.5") || m.contains("gpt-5-5") {
        return CodexRate {
            input: 5.00,
            cached_input: 0.50,
            output: 30.00,
        };
    }
    if m.contains("chat-latest") {
        return CodexRate {
            input: 1.25,
            cached_input: 0.125,
            output: 10.00,
        };
    }
    if m.contains("o3") || m.contains("o4") {
        return CodexRate {
            input: 5.00,
            cached_input: 0.50,
            output: 20.00,
        };
    }

    CodexRate {
        input: 1.25,
        cached_input: 0.125,
        output: 10.00,
    }
}
/// Cost for a single Claude message's token usage.
pub fn claude_message_cost(
    model: &str,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
) -> f64 {
    let r = claude_rate(model);
    (input as f64 / 1_000_000.0) * r.input
        + (output as f64 / 1_000_000.0) * r.output
        + (cache_read as f64 / 1_000_000.0) * r.cache_read
        + (cache_write as f64 / 1_000_000.0) * r.cache_write
}

/// Cost for a Codex turn's token usage.
pub fn codex_turn_cost(
    model: &str,
    input: u64,
    cached_input: u64,
    output: u64,
    reasoning: u64,
) -> f64 {
    let r = codex_rate(model);
    let non_cached_input = input.saturating_sub(cached_input);
    (non_cached_input as f64 / 1_000_000.0) * r.input
        + (cached_input as f64 / 1_000_000.0) * r.cached_input
        + ((output + reasoning) as f64 / 1_000_000.0) * r.output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-9, "expected ~{b}, got {a}");
    }

    // --- Fable/Mythos frontier routing ---
    #[test]
    fn fable_5_pricing() {
        let r = claude_rate("claude-fable-5");
        approx(r.input, 10.00);
        approx(r.output, 50.00);
    }

    #[test]
    fn mythos_label_uses_fable_rate() {
        let r = claude_rate("claude-mythos-5");
        approx(r.input, 10.00);
        approx(r.output, 50.00);
    }

    #[test]
    fn fable_cache_multipliers() {
        let r = claude_rate("claude-fable-5");
        approx(r.cache_read, 1.00);
        approx(r.cache_write, 12.50);
    }

    #[test]
    fn fable_5_composite_cost() {
        // 1M input @ $10 = $10; 100k output @ $50 = $5; total = $15.
        let cost = claude_message_cost("claude-fable-5", 1_000_000, 100_000, 0, 0);
        approx(cost, 15.00);
    }

    // --- Opus generation routing ---
    #[test]
    fn opus_45_is_new_pricing() {
        approx(claude_rate("claude-opus-4-5-20250929").input, 5.00);
    }
    #[test]
    fn opus_46_is_new_pricing() {
        approx(claude_rate("claude-opus-4-6-20251115").input, 5.00);
    }
    #[test]
    fn opus_47_is_new_pricing() {
        approx(claude_rate("claude-opus-4-7-20260301").input, 5.00);
    }
    #[test]
    fn opus_48_is_new_pricing() {
        approx(claude_rate("claude-opus-4-8-20260501").input, 5.00);
    }
    #[test]
    fn opus_41_is_legacy_pricing() {
        approx(claude_rate("claude-opus-4-1-20250805").input, 15.00);
    }
    #[test]
    fn opus_4_is_legacy_pricing() {
        approx(claude_rate("claude-opus-4-20250514").input, 15.00);
    }
    #[test]
    fn opus_3_is_legacy_pricing() {
        approx(claude_rate("claude-3-opus-20240229").input, 15.00);
    }
    #[test]
    fn opus_45_output_25() {
        approx(claude_rate("claude-opus-4-5").output, 25.00);
    }
    #[test]
    fn opus_41_output_75() {
        approx(claude_rate("claude-opus-4-1").output, 75.00);
    }

    // --- Haiku generation routing ---
    #[test]
    fn haiku_45_is_new_pricing() {
        approx(claude_rate("claude-haiku-4-5-20251001").input, 1.00);
    }
    #[test]
    fn haiku_45_output_5() {
        approx(claude_rate("claude-haiku-4-5").output, 5.00);
    }
    #[test]
    fn haiku_35_is_legacy_pricing() {
        approx(claude_rate("claude-3-5-haiku-20241022").input, 0.80);
    }
    #[test]
    fn haiku_35_output_4() {
        approx(claude_rate("claude-3-5-haiku-20241022").output, 4.00);
    }

    // --- Sonnet routing (unchanged across 4 / 4.5 / 4.6) ---
    #[test]
    fn sonnet_45() {
        approx(claude_rate("claude-sonnet-4-5-20250929").input, 3.00);
    }
    #[test]
    fn sonnet_46() {
        approx(claude_rate("claude-sonnet-4-6").input, 3.00);
    }
    #[test]
    fn sonnet_4_deprecated() {
        approx(claude_rate("claude-sonnet-4-20250514").input, 3.00);
    }

    // --- Cache multipliers ---
    #[test]
    fn cache_read_is_10pct() {
        let r = claude_rate("claude-opus-4-5");
        approx(r.cache_read, 0.5);
    }
    #[test]
    fn cache_write_is_125pct() {
        let r = claude_rate("claude-opus-4-5");
        approx(r.cache_write, 6.25);
    }

    // --- Composite cost calculation: 1M input + 100k output on Opus 4.5 ---
    #[test]
    fn opus_45_composite_cost() {
        // 1M input @ $5 = $5; 100k output @ $25 = $2.50; total = $7.50
        let cost = claude_message_cost("claude-opus-4-5", 1_000_000, 100_000, 0, 0);
        approx(cost, 7.50);
    }

    // --- Composite cost on legacy Opus 4.1 (3× higher — the bug we just fixed) ---
    #[test]
    fn opus_41_composite_cost() {
        // 1M input @ $15 = $15; 100k output @ $75 = $7.50; total = $22.50
        let cost = claude_message_cost("claude-opus-4-1", 1_000_000, 100_000, 0, 0);
        approx(cost, 22.50);
    }

    // --- Unknown model falls back safely to Sonnet rate ---
    #[test]
    fn unknown_falls_back_to_sonnet() {
        approx(claude_rate("claude-future-model-20281001").input, 3.00);
    }

    // --- Future Opus 5.x routes to modern pricing ---
    #[test]
    fn opus_5_is_modern() {
        approx(claude_rate("claude-opus-5-0-20270101").input, 5.00);
    }

    // ================================================================
    // Codex / OpenAI pricing tests — guard the SKU routing.
    // ================================================================

    // --- gpt-5.5 family ---
    #[test]
    fn gpt_55_pricing() {
        let r = codex_rate("gpt-5.5");
        approx(r.input, 5.00);
        approx(r.cached_input, 0.50);
        approx(r.output, 30.00);
    }
    #[test]
    fn gpt_55_pro_pricing() {
        let r = codex_rate("gpt-5.5-pro");
        approx(r.input, 30.00);
        approx(r.output, 180.00);
    }

    // --- Current public OpenAI pricing rows ---
    #[test]
    fn gpt_52_codex_pricing() {
        let r = codex_rate("gpt-5.2-codex");
        approx(r.input, 1.75);
        approx(r.cached_input, 0.175);
        approx(r.output, 14.00);
    }
    #[test]
    fn gpt_51_codex_pricing() {
        let r = codex_rate("gpt-5.1-codex");
        approx(r.input, 1.25);
        approx(r.cached_input, 0.125);
        approx(r.output, 10.00);
    }
    #[test]
    fn gpt_5_public_pricing() {
        let r = codex_rate("gpt-5");
        approx(r.input, 1.25);
        approx(r.cached_input, 0.125);
        approx(r.output, 10.00);
    }
    #[test]
    fn gpt_5_mini_pricing() {
        let r = codex_rate("gpt-5-mini");
        approx(r.input, 0.25);
        approx(r.cached_input, 0.025);
        approx(r.output, 2.00);
    }
    #[test]
    fn gpt_5_nano_pricing() {
        let r = codex_rate("gpt-5-nano");
        approx(r.input, 0.05);
        approx(r.cached_input, 0.005);
        approx(r.output, 0.40);
    }
    #[test]
    fn gpt_52_pro_pricing() {
        let r = codex_rate("gpt-5.2-pro");
        approx(r.input, 21.00);
        approx(r.output, 168.00);
    }

    // --- gpt-5.4 family (the bug we just fixed: was $5/$30, should be $2.50/$15) ---
    #[test]
    fn gpt_54_pricing_fix() {
        let r = codex_rate("gpt-5.4");
        approx(r.input, 2.50);
        approx(r.cached_input, 0.25);
        approx(r.output, 15.00);
    }
    #[test]
    fn gpt_54_mini_pricing() {
        let r = codex_rate("gpt-5.4-mini");
        approx(r.input, 0.75);
        approx(r.output, 4.50);
    }
    #[test]
    fn gpt_54_nano_pricing() {
        let r = codex_rate("gpt-5.4-nano");
        approx(r.input, 0.20);
        approx(r.output, 1.25);
    }
    #[test]
    fn gpt_54_pro_pricing() {
        let r = codex_rate("gpt-5.4-pro");
        approx(r.input, 30.00);
        approx(r.output, 180.00);
    }

    // --- Codex CLI-specific SKUs ---
    #[test]
    fn gpt_53_codex_pricing() {
        let r = codex_rate("gpt-5.3-codex");
        approx(r.input, 1.75);
        approx(r.cached_input, 0.175);
        approx(r.output, 14.00);
    }
    #[test]
    fn gpt_5_codex_legacy_alias() {
        let r = codex_rate("gpt-5-codex");
        approx(r.input, 1.25);
        approx(r.output, 10.00);
    }
    #[test]
    fn gpt_53_chat_pricing() {
        let r = codex_rate("gpt-5.3-chat");
        approx(r.input, 1.75);
        approx(r.output, 14.00);
    }

    // --- Variant ordering: -mini / -nano / -pro must NOT fall into bare 5.4 ---
    #[test]
    fn variant_does_not_collapse_into_bare_family() {
        // bare 5.4 = $2.50, mini = $0.75 — if ordering broke these would equal
        assert_ne!(
            codex_rate("gpt-5.4").input,
            codex_rate("gpt-5.4-mini").input
        );
        assert_ne!(
            codex_rate("gpt-5.4").input,
            codex_rate("gpt-5.4-nano").input
        );
        assert_ne!(codex_rate("gpt-5.4").input, codex_rate("gpt-5.4-pro").input);
    }

    // --- Both dash and dot separators recognized ---
    #[test]
    fn dash_separator_for_codex() {
        approx(codex_rate("gpt-5-3-codex").input, 1.75);
    }
    #[test]
    fn dash_separator_for_54_mini() {
        approx(codex_rate("gpt-5-4-mini").input, 0.75);
    }

    // --- Specialized chat-latest passthrough ---
    #[test]
    fn chat_latest_pricing() {
        let r = codex_rate("chat-latest");
        approx(r.input, 1.25);
        approx(r.output, 10.00);
    }

    // --- Composite cost: 1M input + 100k output on gpt-5.4 (the bug fix) ---
    #[test]
    fn gpt_54_composite_cost_fix() {
        // Before: 1M × $5 + 100k × $30 = $5 + $3 = $8 (wrong)
        // After:  1M × $2.50 + 100k × $15 = $2.50 + $1.50 = $4.00 (correct)
        let cost = codex_turn_cost("gpt-5.4", 1_000_000, 0, 100_000, 0);
        approx(cost, 4.00);
    }

    // --- Composite cost: typical Codex CLI turn on gpt-5.3-codex with cache ---
    #[test]
    fn gpt_53_codex_composite_cost_with_cache() {
        // 500k total input (300k cached + 200k fresh) + 50k output + 10k reasoning
        // = 200k × $1.75 + 300k × $0.175 + 60k × $14
        // = $0.35 + $0.0525 + $0.84 = $1.2425
        let cost = codex_turn_cost("gpt-5.3-codex", 500_000, 300_000, 50_000, 10_000);
        approx(cost, 1.2425);
    }

    // --- Unknown model defaults to current public GPT-5 pricing ---
    #[test]
    fn unknown_codex_model_defaults_to_55() {
        let r = codex_rate("gpt-future-2027");
        approx(r.input, 1.25);
        approx(r.output, 10.00);
    }

    // --- Reasoning tokens billed at output rate ---
    #[test]
    fn reasoning_tokens_priced_at_output_rate() {
        // 0 input, 0 output, 100k reasoning @ $30/MTok on gpt-5.5 = $3.00
        let cost = codex_turn_cost("gpt-5.5", 0, 0, 0, 100_000);
        approx(cost, 3.00);
    }
}

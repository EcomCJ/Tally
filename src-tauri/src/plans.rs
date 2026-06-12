// Map vendor-provided plan identifiers to display label + monthly cost (USD).
// Costs are public list prices as of June 2026. Update when vendors change tiers.

#[derive(Debug, Clone)]
pub struct PlanInfo {
    pub label: String,
    pub monthly_cost: f64,
}

/// Anthropic `rate_limit_tier` from /api/oauth/profile
pub fn claude_plan(rate_limit_tier: &str) -> PlanInfo {
    match rate_limit_tier {
        "default_claude_ai" => plan("PRO · $20", 20.0),
        "default_claude_pro" => plan("PRO · $20", 20.0),
        "default_claude_max_5x" => plan("MAX 5× · $100", 100.0),
        "default_claude_max_20x" => plan("MAX 20× · $200", 200.0),
        "default_claude_team" => plan("TEAM · $30", 30.0),
        "default_claude_team_premium" => plan("TEAM PRO · $150", 150.0),
        "default_claude_enterprise" => plan("ENTERPRISE", 0.0),
        // Friendly fallback for unknown tiers: show the raw family so drift is visible.
        other if other.contains("max") => plan("MAX · ?", 100.0),
        other if other.contains("pro") => plan("PRO · ?", 20.0),
        _ => plan("CLAUDE · ?", 0.0),
    }
}

/// Codex/OpenAI `plan_type` from ChatGPT usage and Codex app-server responses.
/// `prolite` is OpenAI's internal id for the 5x tier; `pro` is the 20x tier.
pub fn codex_plan(plan_type: &str) -> PlanInfo {
    match plan_type {
        "free" => plan("FREE", 0.0),
        "plus" => plan("PLUS · $20", 20.0),
        "prolite" => plan("PRO 5× · $100", 100.0),
        "pro" => plan("PRO 20× · $200", 200.0),
        "team" => plan("TEAM", 25.0),
        "enterprise" => plan("ENTERPRISE", 0.0),
        other => plan(&other.to_uppercase(), 0.0),
    }
}

fn plan(label: &str, cost: f64) -> PlanInfo {
    PlanInfo {
        label: label.to_string(),
        monthly_cost: cost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_ai_tier_is_pro_plan() {
        let p = claude_plan("default_claude_ai");
        assert_eq!(p.label, "PRO · $20");
        assert_eq!(p.monthly_cost, 20.0);
    }

    #[test]
    fn codex_prolite_is_5x_100() {
        let p = codex_plan("prolite");
        assert_eq!(p.label, "PRO 5× · $100");
        assert_eq!(p.monthly_cost, 100.0);
    }

    #[test]
    fn codex_pro_is_20x_200() {
        let p = codex_plan("pro");
        assert_eq!(p.label, "PRO 20× · $200");
        assert_eq!(p.monthly_cost, 200.0);
    }
}

pub trait ModelContextLimitResolver {
    fn resolve(&self, model: &str) -> Option<u32>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct StaticModelContextLimitResolver;

fn normalized_model(model: &str) -> String {
    model.trim().to_ascii_lowercase()
}

fn model_suffix(model: &str) -> &str {
    model.split_once('/').map_or(model, |(_, suffix)| suffix)
}

impl ModelContextLimitResolver for StaticModelContextLimitResolver {
    fn resolve(&self, model: &str) -> Option<u32> {
        let normalized = normalized_model(model);
        let suffix = model_suffix(&normalized);

        if suffix.starts_with("gpt-4o-mini") {
            Some(128_000)
        } else if suffix.starts_with("gpt-4o") {
            Some(128_000)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ModelContextLimitResolver, StaticModelContextLimitResolver};

    #[test]
    fn resolves_openai_gpt_4o_family() {
        let resolver = StaticModelContextLimitResolver;

        assert_eq!(resolver.resolve("openai/gpt-4o-mini"), Some(128_000));
        assert_eq!(resolver.resolve("gpt-4o"), Some(128_000));
    }

    #[test]
    fn unknown_model_returns_none() {
        let resolver = StaticModelContextLimitResolver;

        assert_eq!(resolver.resolve("anthropic/claude-sonnet-4-0"), None);
    }
}

//! Bedrock model id → effective estimation profile (design §3.4).

use std::str::FromStr as _;

use super::profile::EstimateProfile;
use crate::types::model_id::BedrockModelId;

/// Refines [`EstimateProfile::BedrockMultiFoundation`] using `BedrockModelId`
/// fields.
#[must_use]
pub fn refine_bedrock_profile(primary_model: &str) -> EstimateProfile {
    let suffix = primary_model
        .split_once('/')
        .map_or(primary_model, |(_, rest)| rest);
    let Ok(id) = BedrockModelId::from_str(suffix) else {
        return EstimateProfile::HeuristicDefault;
    };
    if id.provider.eq_ignore_ascii_case("anthropic") {
        return EstimateProfile::AnthropicMessages;
    }
    let p = id.provider.to_ascii_lowercase();
    if matches!(p.as_str(), "meta" | "mistral" | "ai21" | "amazon") {
        return EstimateProfile::OpenAiCompatible;
    }
    EstimateProfile::HeuristicDefault
}

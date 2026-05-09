use crate::types::provider::InferenceProvider;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCapabilities {
    pub provider: InferenceProvider,
    pub openai_compatible: bool,
    pub supports_response_format: bool,
    pub supports_tool_choice: bool,
    pub supports_parallel_tool_calls: bool,
    pub supports_thinking: bool,
    pub supports_streaming_reasoning: bool,
}

impl ProviderCapabilities {
    #[must_use]
    pub fn for_provider(provider: &InferenceProvider) -> Self {
        super::capability_data::default_provider_capabilities(provider)
    }
}

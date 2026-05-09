use async_openai::types::{CreateCompletionRequest, CreateCompletionResponse};

use crate::{
    endpoints::{AiRequest, Endpoint},
    error::mapper::MapperError,
    types::{model_id::ModelId, provider::InferenceProvider},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Completions;

impl Endpoint for Completions {
    const PATH: &'static str = "v1/completions";
    type RequestBody = CreateCompletionRequest;
    type ResponseBody = CreateCompletionResponse;
    type StreamResponseBody = CreateCompletionResponse;
    type ErrorResponseBody = async_openai::error::WrappedError;
}

impl AiRequest for CreateCompletionRequest {
    fn is_stream(&self) -> bool {
        self.stream.unwrap_or(false)
    }

    fn model(&self) -> Result<ModelId, MapperError> {
        ModelId::from_str_and_provider(InferenceProvider::OpenAI, &self.model)
    }
}

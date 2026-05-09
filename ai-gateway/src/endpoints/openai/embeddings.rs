use async_openai::types::{CreateEmbeddingRequest, CreateEmbeddingResponse};

use crate::{
    endpoints::{AiRequest, Endpoint},
    error::mapper::MapperError,
    types::{model_id::ModelId, provider::InferenceProvider},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Embeddings;

impl Endpoint for Embeddings {
    const PATH: &'static str = "v1/embeddings";
    type RequestBody = CreateEmbeddingRequest;
    type ResponseBody = CreateEmbeddingResponse;
    type StreamResponseBody = CreateEmbeddingResponse;
    type ErrorResponseBody = async_openai::error::WrappedError;
}

impl AiRequest for CreateEmbeddingRequest {
    fn is_stream(&self) -> bool {
        false
    }

    fn model(&self) -> Result<ModelId, MapperError> {
        ModelId::from_str_and_provider(InferenceProvider::OpenAI, &self.model)
    }
}

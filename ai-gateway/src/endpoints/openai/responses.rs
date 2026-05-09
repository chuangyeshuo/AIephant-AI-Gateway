use async_openai::types::responses::{CreateResponse, Response};

use crate::{
    endpoints::{AiRequest, Endpoint},
    error::mapper::MapperError,
    types::{model_id::ModelId, provider::InferenceProvider},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Responses;

impl Endpoint for Responses {
    const PATH: &'static str = "v1/responses";
    type RequestBody = CreateResponse;
    type ResponseBody = Response;
    type StreamResponseBody = Response;
    type ErrorResponseBody = async_openai::error::WrappedError;
}

impl AiRequest for CreateResponse {
    fn is_stream(&self) -> bool {
        self.stream.unwrap_or(false)
    }

    fn model(&self) -> Result<ModelId, MapperError> {
        ModelId::from_str_and_provider(InferenceProvider::OpenAI, &self.model)
    }
}

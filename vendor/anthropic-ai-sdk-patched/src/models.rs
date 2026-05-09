//! Models API
//!
//! This module contains the implementations for the Anthropic Models API endpoints.
//! It provides functionality for listing available models and their capabilities.

use crate::client::AnthropicClient;
use crate::types::model::{ListModelsParams, ListModelsResponse, Model, ModelClient, ModelError};
use async_trait::async_trait;

#[async_trait]
impl ModelClient for AnthropicClient {
    /// Lists available models
    ///
    /// Retrieves a list of models that are available through the API.
    /// The response includes model details such as ID, display name, and capabilities.
    /// Models are sorted by release date, with more recently released models listed first.
    ///
    /// # Arguments
    ///
    /// * `params` - Optional parameters for pagination and filtering the results
    ///
    /// # Returns
    ///
    /// Returns a list of available models and pagination information on success.
    ///
    /// # Errors
    ///
    /// Returns a `ModelError` if:
    /// - The request fails to send
    /// - The API returns an error response
    /// - The response cannot be parsed
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use anthropic_ai_sdk::client::AnthropicClient;
    /// use anthropic_ai_sdk::types::model::{ModelClient, ModelError};
    /// use tokio;
    ///
    /// #[tokio::main]
    /// async fn main() -> Result<(), ModelError> {
    ///     let client = AnthropicClient::new::<ModelError>(
    ///         "your-api-key",
    ///         "2023-06-01",
    ///     )?;
    ///
    ///     // List all available models
    ///     let models = client.list_models(None).await?;
    ///     for model in models.data {
    ///         println!("Model: {} ({})", model.display_name, model.id);
    ///     }
    ///
    ///     Ok(())
    /// }
    /// ```
    async fn list_models<'a>(
        &'a self,
        params: Option<&'a ListModelsParams>,
    ) -> Result<ListModelsResponse, ModelError> {
        self.get("/models", params).await
    }

    /// Get a model
    ///
    /// Retrieves detailed information about a specific model, including its capabilities and parameters.
    ///
    /// # Arguments
    ///
    /// * `model_id` - The ID of the model to retrieve
    ///
    /// # Returns
    ///
    /// Returns the model's details on success.
    ///
    /// # Errors
    ///
    /// Returns a `ModelError` if:
    /// - The request fails to send
    /// - The API returns an error response
    /// - The response cannot be parsed or is not a valid model
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use anthropic_ai_sdk::client::AnthropicClient;
    /// use anthropic_ai_sdk::types::model::{ModelClient, ModelError};
    /// use tokio;
    ///
    /// #[tokio::main]
    /// async fn main() -> Result<(), ModelError> {
    ///     let client = AnthropicClient::new::<ModelError>(
    ///         "your-api-key",
    ///         "2023-06-01",
    ///     )?;
    ///
    ///     // Get a model
    ///     let model = client.get_model("claude-3-5-sonnet-20240620").await?;
    ///     println!("Model: {}", model.display_name);
    ///
    ///     Ok(())
    /// }
    /// ```
    async fn get_model<'a>(&'a self, model_id: &'a str) -> Result<Model, ModelError> {
        self.get(&format!("/models/{}", model_id), Option::<&()>::None)
            .await
    }
}

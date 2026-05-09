//! Message Batches API
//!
//! This module contains the types and functions for the Anthropic Message Batches API.
//!
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use time::serde::rfc3339;

/// Error types for the Message Batches API
#[derive(Debug, Error)]
pub enum MessageBatchError {
    #[error("Batch size exceeds maximum limit of 100,000 requests")]
    BatchTooLarge,
    #[error("Batch total size exceeds 256MB")]
    BatchSizeExceeded,
    #[error("API request failed: {0}")]
    RequestFailed(String),
    #[error("API error: {0}")]
    ApiError(String),
}

impl From<String> for MessageBatchError {
    fn from(error: String) -> Self {
        MessageBatchError::ApiError(error)
    }
}

#[async_trait]
pub trait MessageBatchClient {
    /// Create a new message batch
    async fn create_message_batch<'a>(
        &'a self,
        params: &'a CreateMessageBatchParams,
    ) -> Result<MessageBatch, MessageBatchError>;

    /// List message batches
    async fn list_message_batches<'a>(
        &'a self,
        params: Option<&'a ListMessageBatchesParams>,
    ) -> Result<ListMessageBatchesResponse, MessageBatchError>;

    /// Retrieve a message batch
    async fn retrieve_message_batch<'a>(
        &'a self,
        params: &'a RetrieveMessageBatchParams,
    ) -> Result<RetrieveMessageBatchResponse, MessageBatchError>;

    async fn retrieve_message_batch_results<'a>(
        &'a self,
        params: &'a RetrieveMessageBatchResultsParams,
    ) -> Result<RetrieveMessageBatchResultsResponse, MessageBatchError>;

    /// Cancel a message batch
    async fn cancel_message_batch<'a>(
        &'a self,
        params: &'a CancelMessageBatchParams,
    ) -> Result<CancelResponse, MessageBatchError>;

    /// Delete a message batch.
    ///
    /// Message batches can only be deleted once they’ve finished processing.
    /// If you’d like to delete an in-progress batch, you must first cancel it.
    async fn delete_message_batch<'a>(
        &'a self,
        params: &'a DeleteMessageBatchParams,
    ) -> Result<DeleteResponse, MessageBatchError>;
}

/// Processing status of a Message Batch
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingStatus {
    InProgress,
    Canceling,
    Ended,
}

/// Request counts for different statuses
#[derive(Debug, Serialize, Deserialize)]
pub struct RequestCounts {
    /// Number of requests currently processing
    pub processing: u32,
    /// Number of successfully completed requests
    pub succeeded: u32,
    /// Number of errored requests
    pub errored: u32,
    /// Number of canceled requests
    pub canceled: u32,
    /// Number of expired requests
    pub expired: u32,
}

/// Response structure for Message Batch creation
#[derive(Debug, Serialize, Deserialize)]
pub struct MessageBatch {
    /// Unique identifier for the batch
    pub id: String,
    /// Object type (always "message_batch")
    #[serde(rename = "type")]
    pub type_: String,
    /// Time when the batch was created
    #[serde(with = "rfc3339")]
    pub created_at: OffsetDateTime,
    /// Time when the batch will expire
    #[serde(with = "rfc3339")]
    pub expires_at: OffsetDateTime,
    /// Time when the batch was archived (if applicable)
    #[serde(with = "rfc3339::option")]
    pub archived_at: Option<OffsetDateTime>,
    /// Time when cancellation was initiated (if applicable)
    #[serde(with = "rfc3339::option")]
    pub cancel_initiated_at: Option<OffsetDateTime>,
    /// Time when processing ended (if applicable)
    #[serde(with = "rfc3339::option")]
    pub ended_at: Option<OffsetDateTime>,
    /// Current processing status
    pub processing_status: ProcessingStatus,
    /// Counts of requests in different states
    pub request_counts: RequestCounts,
    /// URL to download results (once processing ends)
    pub results_url: Option<String>,
}

/// Parameters for creating a message batch
#[derive(Debug, Serialize)]
pub struct CreateMessageBatchParams {
    /// List of message creation requests
    pub requests: Vec<MessageRequest>,
}

/// Individual message request within a batch
#[derive(Debug, Serialize)]
pub struct MessageRequest {
    /// Custom identifier for tracking this request
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_id: Option<String>,
    /// Request parameters
    pub params: MessageRequestParams,
}

/// Parameters for an individual message request
#[derive(Debug, Serialize)]
pub struct MessageRequestParams {
    /// Model to use for this message
    pub model: String,
    /// Maximum number of tokens to generate
    pub max_tokens: u32,
    /// System prompt for the message
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// User message content
    pub messages: Vec<Message>,
}

/// Message content
#[derive(Debug, Serialize)]
pub struct Message {
    /// Role of the message sender
    pub role: String,
    /// Content of the message
    pub content: String,
}

impl CreateMessageBatchParams {
    /// Create a new CreateMessageBatchParams with the given requests
    pub fn new(requests: Vec<MessageRequest>) -> Self {
        if requests.len() > 100_000 {
            panic!("Batch size exceeds maximum limit of 100,000 requests");
        }
        Self { requests }
    }
}

impl MessageRequest {
    /// Create a new MessageRequest
    pub fn new(params: MessageRequestParams) -> Self {
        Self {
            custom_id: None,
            params,
        }
    }

    /// Set a custom ID for this request
    pub fn with_custom_id(mut self, custom_id: impl Into<String>) -> Self {
        self.custom_id = Some(custom_id.into());
        self
    }
}

impl MessageRequestParams {
    /// Create a new MessageRequestParams
    pub fn new(model: impl Into<String>, messages: Vec<Message>, max_tokens: u32) -> Self {
        Self {
            model: model.into(),
            max_tokens,
            system: None,
            messages,
        }
    }

    /// Set a system prompt for this request
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }
}

impl Message {
    /// Create a new Message
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }
}

/// Response for listing message batches
#[derive(Debug, Serialize, Deserialize)]
pub struct ListMessageBatchesResponse {
    /// List of message batches
    pub data: Vec<MessageBatch>,
    /// First ID in the data list
    pub first_id: Option<String>,
    /// Last ID in the data list
    pub last_id: Option<String>,
    /// Indicates if there are more results
    pub has_more: bool,
}

/// Parameters for listing message batches
#[derive(Debug, Serialize, Default)]
pub struct ListMessageBatchesParams {
    /// Cursor for pagination (before)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_id: Option<String>,
    /// Cursor for pagination (after)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_id: Option<String>,
    /// Number of items per page (1-1000)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u16>,
}

impl ListMessageBatchesParams {
    /// Create a new ListMessageBatchesParams with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the before_id parameter
    pub fn before_id(mut self, before_id: impl Into<String>) -> Self {
        self.before_id = Some(before_id.into());
        self
    }

    /// Set the after_id parameter
    pub fn after_id(mut self, after_id: impl Into<String>) -> Self {
        self.after_id = Some(after_id.into());
        self
    }

    /// Set the limit parameter (1-1000)
    pub fn limit(mut self, limit: u16) -> Self {
        self.limit = Some(limit.clamp(1, 1000));
        self
    }
}

/// Parameters for retrieving a message batch
#[derive(Debug, Serialize)]
pub struct RetrieveMessageBatchParams {
    /// ID of the message batch to retrieve
    pub message_batch_id: String,
}

impl RetrieveMessageBatchParams {
    /// Create a new RetrieveMessageBatchParams
    pub fn new(message_batch_id: impl Into<String>) -> Self {
        Self {
            message_batch_id: message_batch_id.into(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RetrieveMessageBatchResultsParams {
    /// ID of the message batch to retrieve
    pub message_batch_id: String,
}

impl RetrieveMessageBatchResultsParams {
    /// Create a new RetrieveMessageBatchResultsParams
    pub fn new(message_batch_id: impl Into<String>) -> Self {
        Self {
            message_batch_id: message_batch_id.into(),
        }
    }
}

/// Response type for retrieving a message batch
pub type RetrieveMessageBatchResponse = MessageBatch;

#[derive(Debug, Serialize, Deserialize)]
pub struct MessageBatchResult {
    /// Custom identifier provided in the original request
    pub custom_id: String,
    /// Result of the message request
    pub result: BatchRequestResult,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BatchRequestResult {
    /// Type of result (e.g., "succeeded")
    #[serde(rename = "type")]
    pub type_: String,
    /// The resulting message if successful
    pub message: MessageResponse,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MessageResponse {
    /// Unique identifier for the message
    pub id: String,
    /// Type of the response (always "message")
    #[serde(rename = "type")]
    pub type_: String,
    /// Role of the message (e.g., "assistant")
    pub role: String,
    /// Model used for generation
    pub model: String,
    /// Content of the message
    pub content: Vec<MessageContent>,
    /// Reason for stopping generation
    pub stop_reason: String,
    /// Sequence that caused the stop
    pub stop_sequence: Option<String>,
    /// Token usage statistics
    pub usage: TokenUsage,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MessageContent {
    /// Type of content (e.g., "text")
    #[serde(rename = "type")]
    pub type_: String,
    /// The actual text content
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Number of tokens in the input
    pub input_tokens: u32,
    /// Number of tokens in the output
    pub output_tokens: u32,
}

/// Response type for retrieving message batch results
/// This will be a stream of MessageBatchResult objects, one per line
pub type RetrieveMessageBatchResultsResponse = Vec<MessageBatchResult>;

#[derive(Debug, Serialize, Deserialize)]
pub struct CancelMessageBatchParams {
    pub message_batch_id: String,
}

impl CancelMessageBatchParams {
    pub fn new(message_batch_id: impl Into<String>) -> Self {
        Self {
            message_batch_id: message_batch_id.into(),
        }
    }
}

/// Response type for cancelling a message batch
#[derive(Debug, Deserialize)]
pub struct CancelResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub obj_type: String,
    pub processing_status: String,
    pub request_counts: serde_json::Value,
    pub ended_at: Option<String>,
    pub created_at: String,
    pub expires_at: String,
    pub archived_at: Option<String>,
    pub cancel_initiated_at: Option<String>,
    pub results_url: Option<String>,
}

/// Parameters for deleting a message batch
#[derive(Debug, Serialize)]
pub struct DeleteMessageBatchParams {
    /// ID of the message batch to delete
    pub message_batch_id: String,
}

impl DeleteMessageBatchParams {
    /// Create a new DeleteMessageBatchParams
    pub fn new(message_batch_id: impl Into<String>) -> Self {
        Self {
            message_batch_id: message_batch_id.into(),
        }
    }
}

/// Response type for deleting a message batch
#[derive(Debug, Deserialize)]
pub struct DeleteResponse {
    /// Unique identifier for the deleted message batch
    pub id: String,
    /// Object type (always "message_batch_deleted")
    #[serde(rename = "type")]
    pub obj_type: String,
}

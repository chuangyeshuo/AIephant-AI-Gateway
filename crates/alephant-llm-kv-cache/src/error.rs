use thiserror::Error;

#[derive(Debug, Error)]
pub enum LlmKvCacheError {
    #[error("http: {0}")]
    Http(String),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("invalid config: {0}")]
    Config(String),
    #[error("tikv: {0}")]
    Tikv(String),
}

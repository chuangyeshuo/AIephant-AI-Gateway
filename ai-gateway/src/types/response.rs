use axum_core::body::Body;
use serde::{Deserialize, Serialize};

pub type Response = http::Response<Body>;

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged, rename_all = "camelCase")]
pub enum AlephantApiResponse<T> {
    Data { data: T },
    Error { error: String },
}

impl<T> AlephantApiResponse<T> {
    pub fn data(self) -> Result<T, String> {
        match self {
            AlephantApiResponse::Data { data } => Ok(data),
            AlephantApiResponse::Error { error } => Err(error),
        }
    }
}

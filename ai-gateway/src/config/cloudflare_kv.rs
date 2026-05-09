use serde::{Deserialize, Serialize};

use crate::types::secret::Secret;

/// Cloudflare KV REST (`external` / `--features external`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct CloudflareKvConfig {
    /// e.g. `https://api.cloudflare.com/client/v4` or a corporate proxy prefix.
    pub api_base: String,
    pub account_id: String,
    pub namespace_id: String,
    pub api_token: Secret<String>,
}

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LlmCacheEntry {
    pub headers: HashMap<String, String>,
    pub latency: u64,
    pub body: Vec<String>,
}

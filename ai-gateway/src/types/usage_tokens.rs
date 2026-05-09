//! Token / usage counters aligned with provider `usage` JSON and RMT naming.
//!
//! Used by [`crate::logger::usage_parse`]。

/// Token and usage counters aligned with logs-collector / RMT naming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UsageTokenCounts {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub prompt_cache_write_tokens: i64,
    pub prompt_cache_read_tokens: i64,
    pub prompt_audio_tokens: i64,
    pub completion_audio_tokens: i64,
    pub reasoning_tokens: i64,
}

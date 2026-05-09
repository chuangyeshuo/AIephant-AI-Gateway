//! Extract OpenAI-compatible `usage` counters from a provider response body
//! (JSON) or from SSE `data:` frames (streaming logs).
//!
//! Mapping (`OpenAI` Chat Completions / Responses-style objects):
//! - `usage.prompt_tokens` → [`UsageTokenCounts::prompt_tokens`]
//! - `usage.completion_tokens` → [`UsageTokenCounts::completion_tokens`]
//! - `usage.prompt_tokens_details.cached_tokens` →
//!   [`UsageTokenCounts::prompt_cache_read_tokens`]
//! - `usage.prompt_tokens_details.cache_write_tokens` →
//!   [`UsageTokenCounts::prompt_cache_write_tokens`] (extension field)
//! - `usage.prompt_tokens_details.audio_tokens` →
//!   [`UsageTokenCounts::prompt_audio_tokens`]
//! - `usage.completion_tokens_details.audio_tokens` →
//!   [`UsageTokenCounts::completion_audio_tokens`]
//! - `usage.completion_tokens_details.reasoning_tokens` →
//!   [`UsageTokenCounts::reasoning_tokens`]
//!
//! `prompt_cache_read_tokens` comes from `cached_tokens` (`OpenAI`) or
//! `cache_read_input_tokens` (Bedrock-style). `prompt_cache_write_tokens` from
//! `cache_write_input_tokens` when present, else `cache_write_tokens` when
//! present; otherwise **0**.

use serde_json::Value;

pub use crate::types::usage_tokens::UsageTokenCounts;

/// Parse `usage` from a single JSON document (UTF-8). On any error, returns
/// [`UsageTokenCounts::default`]. Equivalent to
/// [`usage_counts_from_response_body_for_log`] with `is_stream: false` and no
/// SSE fallback when the root object has no `usage`.
#[must_use]
pub fn usage_counts_from_response_body(body: &[u8]) -> UsageTokenCounts {
    usage_counts_from_response_body_for_log(false, body)
}

/// Prefer a single JSON root when `is_stream` is false and `usage` is present
/// and non-zero; otherwise scan `data:` SSE lines and take the **last** frame
/// that carries a `usage` object.
#[must_use]
pub fn usage_counts_from_response_body_for_log(is_stream: bool, body: &[u8]) -> UsageTokenCounts {
    let from_single = std::str::from_utf8(body)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(t).ok())
        .map(|v| extract_usage_from_root(&v))
        .unwrap_or_default();

    if !is_stream && from_single != UsageTokenCounts::default() {
        return from_single;
    }

    let from_sse = usage_counts_from_sse_data_frames(body);
    if from_sse != UsageTokenCounts::default() {
        return from_sse;
    }

    from_single
}

/// Scan UTF-8 for lines starting with `data:`; parse each payload as JSON and
/// keep the last [`UsageTokenCounts`] derived from a `usage` field.
#[must_use]
pub fn usage_counts_from_sse_data_frames(body: &[u8]) -> UsageTokenCounts {
    let Ok(s) = std::str::from_utf8(body) else {
        return UsageTokenCounts::default();
    };
    let mut last = UsageTokenCounts::default();
    for line in s.lines() {
        let line = line.trim();
        let payload = if let Some(rest) = line.strip_prefix("data:") {
            rest.trim()
        } else {
            continue;
        };
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(payload) else {
            continue;
        };
        if let Some(u) = v.get("usage").filter(|x| !x.is_null()) {
            last = extract_usage_from_usage_object(u);
        }
    }
    last
}

#[must_use]
pub fn extract_usage_from_root(root: &Value) -> UsageTokenCounts {
    let Some(usage) = root.get("usage") else {
        return UsageTokenCounts::default();
    };
    if usage.is_null() {
        return UsageTokenCounts::default();
    }
    extract_usage_from_usage_object(usage)
}

fn extract_usage_from_usage_object(usage: &Value) -> UsageTokenCounts {
    let mut out = UsageTokenCounts {
        prompt_tokens: json_i64(usage, "prompt_tokens"),
        completion_tokens: json_i64(usage, "completion_tokens"),
        ..Default::default()
    };

    if let Some(details) = usage.get("prompt_tokens_details") {
        out.prompt_cache_read_tokens = json_i64(details, "cached_tokens");
        // Some stacks expose audio under prompt details.
        out.prompt_audio_tokens = json_i64(details, "audio_tokens");
        // Bedrock-style nested cache (read vs write) — best-effort.
        if out.prompt_cache_read_tokens == 0
            && let Some(cache) = details.get("cache_read_input_tokens")
        {
            out.prompt_cache_read_tokens = value_as_i64(cache);
        }
        if out.prompt_cache_write_tokens == 0
            && let Some(cache) = details.get("cache_write_input_tokens")
        {
            out.prompt_cache_write_tokens = value_as_i64(cache);
        }
        if out.prompt_cache_write_tokens == 0 {
            out.prompt_cache_write_tokens = json_i64(details, "cache_write_tokens");
        }
    }

    if let Some(details) = usage.get("completion_tokens_details") {
        out.reasoning_tokens = json_i64(details, "reasoning_tokens");
        out.completion_audio_tokens = json_i64(details, "audio_tokens");
    }

    out
}

fn json_i64(obj: &Value, key: &str) -> i64 {
    obj.get(key).map_or(0, value_as_i64)
}

fn value_as_i64(v: &Value) -> i64 {
    v.as_i64()
        .or_else(|| v.as_u64().map(|u| i64::try_from(u).unwrap_or(i64::MAX)))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_parse_invalid_utf8_yields_zeros() {
        let counts = usage_counts_from_response_body(&[0xFF, 0xFE]);
        assert_eq!(counts, UsageTokenCounts::default());
    }

    #[test]
    fn usage_parse_non_json_yields_zeros() {
        let counts = usage_counts_from_response_body(b"not json");
        assert_eq!(counts, UsageTokenCounts::default());
    }

    #[test]
    fn usage_parse_openai_completion_usage() {
        let json = r#"{"usage":{"prompt_tokens":10,"completion_tokens":20,"total_tokens":30}}"#;
        let c = usage_counts_from_response_body(json.as_bytes());
        assert_eq!(c.prompt_tokens, 10);
        assert_eq!(c.completion_tokens, 20);
        assert_eq!(c.prompt_cache_read_tokens, 0);
    }

    #[test]
    fn usage_parse_cached_and_reasoning_details() {
        let json = r#"{"usage":{"prompt_tokens":100,"completion_tokens":50,"prompt_tokens_details":{"cached_tokens":40,"audio_tokens":2},"completion_tokens_details":{"reasoning_tokens":10,"audio_tokens":3}}}"#;
        let c = usage_counts_from_response_body(json.as_bytes());
        assert_eq!(c.prompt_tokens, 100);
        assert_eq!(c.completion_tokens, 50);
        assert_eq!(c.prompt_cache_read_tokens, 40);
        assert_eq!(c.prompt_audio_tokens, 2);
        assert_eq!(c.reasoning_tokens, 10);
        assert_eq!(c.completion_audio_tokens, 3);
    }

    #[test]
    fn usage_parse_bedrock_style_cache_split() {
        let json = r#"{"usage":{"prompt_tokens":1,"completion_tokens":2,"prompt_tokens_details":{"cache_read_input_tokens":50,"cache_write_input_tokens":60}}}"#;
        let c = usage_counts_from_response_body(json.as_bytes());
        assert_eq!(c.prompt_cache_read_tokens, 50);
        assert_eq!(c.prompt_cache_write_tokens, 60);
    }

    #[test]
    fn usage_from_sse_log_body_picks_last_usage() {
        let frames = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"\
             completion_tokens\":7,\"total_tokens\":10,\"\
             prompt_tokens_details\":{\"cached_tokens\":2,\"\
             cache_write_tokens\":5,\"cache_write_details\":{\"\
             write_5m_tokens\":5,\"write_1h_tokens\":0}}}}\n\n",
        );
        let c = usage_counts_from_response_body_for_log(false, frames.as_bytes());
        assert_eq!(c.prompt_tokens, 3);
        assert_eq!(c.completion_tokens, 7);
        assert_eq!(c.prompt_cache_read_tokens, 2);
        assert_eq!(c.prompt_cache_write_tokens, 5);
    }
}

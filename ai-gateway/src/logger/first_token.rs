//! Heuristics for “first model token” in streamed LLM responses (TTFT).

/// Returns `true` when `bytes` is JSON from a provider stream chunk that
/// carries the first non-empty model text (OpenAI-style `delta.content` /
/// `delta.reasoning_content`, or Anthropic `content_block_delta`).
#[must_use]
pub fn chunk_has_first_model_token(bytes: &[u8]) -> bool {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };

    if let Some(choices) = v.get("choices").and_then(|c| c.as_array()) {
        for choice in choices {
            let Some(delta) = choice.get("delta").and_then(|d| d.as_object()) else {
                continue;
            };
            for key in ["content", "reasoning_content"] {
                if let Some(s) = delta.get(key).and_then(|x| x.as_str())
                    && !s.is_empty()
                {
                    return true;
                }
            }
        }
        return false;
    }

    if v.get("type").and_then(|t| t.as_str()) == Some("content_block_delta")
        && let Some(text) = v
            .pointer("/delta/text")
            .and_then(|t| t.as_str())
            .or_else(|| {
                v.get("delta")
                    .and_then(|d| d.get("text"))
                    .and_then(|t| t.as_str())
            })
    {
        return !text.is_empty();
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_empty_delta_is_not_first_token() {
        let j = br#"{"choices":[{"delta":{"role":"assistant"}}]}"#;
        assert!(!chunk_has_first_model_token(j));
    }

    #[test]
    fn openai_content_delta_is_first_token() {
        let j = br#"{"choices":[{"delta":{"content":"hi"}}]}"#;
        assert!(chunk_has_first_model_token(j));
    }

    #[test]
    fn anthropic_text_delta_is_first_token() {
        let j =
            br#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}"#;
        assert!(chunk_has_first_model_token(j));
    }
}

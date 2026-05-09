//! Bridge OpenAI Responses API payloads to Chat Completions shape for unified
//! `chat/completions` clients (e.g. Cursor) that POST Responses-shaped JSON but
//! expect Chat Completions SSE / JSON.

use async_openai::types::{
    ChatChoice, ChatChoiceStream, ChatCompletionResponseMessage,
    ChatCompletionStreamResponseDelta, CompletionUsage,
    CreateChatCompletionResponse, CreateChatCompletionStreamResponse,
    FinishReason, Role,
    responses::{Content, OutputContent, Response, Status},
};
use bytes::{BufMut, Bytes, BytesMut};
use serde::Serialize;
use serde_json::Value;

use crate::{
    error::{api::ApiError, internal::InternalError},
    middleware::mapper::stream_normalizer::{
        build_finish_choice, build_role_choice, build_stream_response,
        build_text_choice,
    },
};

#[derive(Debug, Default)]
pub(super) struct BridgeStreamState {
    completion_id: Option<String>,
    model: Option<String>,
    role_sent: bool,
}

fn completion_usage_from_responses_value(u: &Value) -> Option<CompletionUsage> {
    let input = u.get("input_tokens")?.as_u64()? as u32;
    let output = u.get("output_tokens")?.as_u64()? as u32;
    let total = u
        .get("total_tokens")
        .and_then(serde_json::Value::as_u64)
        .map(|t| t as u32)
        .unwrap_or_else(|| input.saturating_add(output));
    Some(CompletionUsage {
        prompt_tokens: input,
        completion_tokens: output,
        total_tokens: total,
        prompt_tokens_details: None,
        completion_tokens_details: None,
    })
}

fn put_sse_record(buf: &mut BytesMut, payload: &[u8]) {
    buf.put("data: ".as_bytes());
    buf.put(payload);
    buf.put("\n\n".as_bytes());
}

fn put_sse_json<T: Serialize>(
    buf: &mut BytesMut,
    val: &T,
) -> Result<(), ApiError> {
    let json = serde_json::to_vec(val).map_err(|error| {
        ApiError::Internal(InternalError::Serialize {
            ty: std::any::type_name::<T>(),
            error,
        })
    })?;
    put_sse_record(buf, &json);
    Ok(())
}

impl BridgeStreamState {
    fn ingest_response_snapshot(&mut self, resp: Option<&Value>) {
        let Some(resp) = resp else {
            return;
        };
        if let Some(id) = resp.get("id").and_then(|i| i.as_str()) {
            self.completion_id = Some(id.to_string());
        }
        if let Some(m) = resp.get("model").and_then(|m| m.as_str()) {
            self.model = Some(m.to_string());
        }
    }

    fn completion_id(&self) -> String {
        self.completion_id
            .clone()
            .unwrap_or_else(|| "chatcmpl-bridge".to_string())
    }

    fn model_name(&self) -> String {
        self.model.clone().unwrap_or_else(|| "unknown".to_string())
    }

    fn push_role_if_needed(
        &mut self,
        buf: &mut BytesMut,
    ) -> Result<(), ApiError> {
        if self.role_sent {
            return Ok(());
        }
        let chunk = build_stream_response(
            self.completion_id(),
            self.model_name(),
            vec![build_role_choice(0, Role::Assistant)],
            None,
        );
        put_sse_json(buf, &chunk)?;
        self.role_sent = true;
        Ok(())
    }

    fn finish_chunk(
        &self,
        usage: Option<CompletionUsage>,
    ) -> Result<CreateChatCompletionStreamResponse, ApiError> {
        Ok(build_stream_response(
            self.completion_id(),
            self.model_name(),
            vec![build_finish_choice(
                Some(FinishReason::Stop),
                CompletionUsage::default(),
                None,
            )],
            usage,
        ))
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn process_upstream_sse_json(
        &mut self,
        raw: &[u8],
    ) -> Result<Option<Bytes>, ApiError> {
        let v: Value = serde_json::from_slice(raw).map_err(|error| {
            ApiError::Internal(InternalError::Deserialize {
                ty: "responses_sse_event",
                error,
            })
        })?;

        let mut buf = BytesMut::new();
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

        match ty {
            "response.created" | "response.in_progress" => {
                self.ingest_response_snapshot(v.get("response"));
            }
            "response.output_text.delta"
            | "response.reasoning_text.delta"
            | "response.reasoning_summary_text.delta" => {
                let delta =
                    v.get("delta").and_then(|d| d.as_str()).unwrap_or("");
                if delta.is_empty() {
                    return Ok(None);
                }
                self.push_role_if_needed(&mut buf)?;
                let chunk = build_stream_response(
                    self.completion_id(),
                    self.model_name(),
                    vec![build_text_choice(0, delta.to_string())],
                    None,
                );
                put_sse_json(&mut buf, &chunk)?;
            }
            "response.refusal.delta" => {
                let delta =
                    v.get("delta").and_then(|d| d.as_str()).unwrap_or("");
                if delta.is_empty() {
                    return Ok(None);
                }
                self.push_role_if_needed(&mut buf)?;
                let choice = ChatChoiceStream {
                    index: 0,
                    delta: ChatCompletionStreamResponseDelta {
                        content: None,
                        #[allow(deprecated)]
                        function_call: None,
                        tool_calls: None,
                        role: None,
                        refusal: Some(delta.to_string()),
                    },
                    finish_reason: None,
                    logprobs: None,
                };
                let chunk = build_stream_response(
                    self.completion_id(),
                    self.model_name(),
                    vec![choice],
                    None,
                );
                put_sse_json(&mut buf, &chunk)?;
            }
            "response.completed" => {
                self.ingest_response_snapshot(v.get("response"));
                self.push_role_if_needed(&mut buf)?;
                let usage = v
                    .get("response")
                    .and_then(|r| r.get("usage"))
                    .and_then(completion_usage_from_responses_value);
                let finish = self.finish_chunk(usage)?;
                put_sse_json(&mut buf, &finish)?;
                put_sse_record(&mut buf, b"[DONE]");
            }
            "error" => {
                let err_obj = serde_json::json!({
                    "error": {
                        "message": v.get("message").and_then(|m| m.as_str()).unwrap_or("unknown error"),
                        "type": "invalid_request_error",
                        "code": v.get("code").and_then(|c| c.as_str()),
                        "param": v.get("param").and_then(|p| p.as_str()),
                    }
                });
                put_sse_json(&mut buf, &err_obj)?;
            }
            "response.failed" => {
                self.ingest_response_snapshot(v.get("response"));
                self.push_role_if_needed(&mut buf)?;
                let finish = self.finish_chunk(None)?;
                put_sse_json(&mut buf, &finish)?;
                put_sse_record(&mut buf, b"[DONE]");
            }
            _ => {
                return Ok(None);
            }
        }

        if buf.is_empty() {
            Ok(None)
        } else {
            Ok(Some(buf.freeze()))
        }
    }
}

fn aggregate_output_text(resp: &Response) -> (Option<String>, Option<String>) {
    if let Some(ref t) = resp.output_text {
        return (Some(t.clone()), None);
    }
    let mut text_buf = String::new();
    let mut refusal_out: Option<String> = None;
    for item in &resp.output {
        match item {
            OutputContent::Message(m) => {
                for c in &m.content {
                    match c {
                        Content::OutputText(ot) => text_buf.push_str(&ot.text),
                        Content::Refusal(r) => {
                            refusal_out = Some(r.refusal.clone());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let content = (!text_buf.is_empty()).then_some(text_buf);
    (content, refusal_out)
}

fn finish_reason_for_status(status: &Status) -> Option<FinishReason> {
    match status {
        Status::Completed => Some(FinishReason::Stop),
        Status::Incomplete => Some(FinishReason::Length),
        Status::Failed => Some(FinishReason::Stop),
        Status::InProgress => None,
    }
}

pub(super) fn non_stream_responses_body_to_chat_completion(
    body: &[u8],
) -> Result<Bytes, ApiError> {
    let resp: Response = serde_json::from_slice(body).map_err(|error| {
        ApiError::Internal(InternalError::Deserialize {
            ty: std::any::type_name::<Response>(),
            error,
        })
    })?;

    let (content, refusal) = aggregate_output_text(&resp);
    let finish_reason = finish_reason_for_status(&resp.status);

    let message = ChatCompletionResponseMessage {
        content,
        refusal,
        tool_calls: None,
        role: Role::Assistant,
        #[allow(deprecated)]
        function_call: None,
        audio: None,
    };

    let choice = ChatChoice {
        index: 0,
        message,
        finish_reason,
        logprobs: None,
    };

    let usage = resp.usage.as_ref().map(|u| CompletionUsage {
        prompt_tokens: u.input_tokens,
        completion_tokens: u.output_tokens,
        total_tokens: u.total_tokens,
        prompt_tokens_details: None,
        completion_tokens_details: None,
    });

    let created_u32 = u32::try_from(resp.created_at).unwrap_or(u32::MAX);

    let out = CreateChatCompletionResponse {
        id: resp.id.clone(),
        choices: vec![choice],
        created: created_u32,
        model: resp.model.clone(),
        service_tier: None,
        system_fingerprint: None,
        object: "chat.completion".to_string(),
        usage,
    };

    let bytes = serde_json::to_vec(&out).map_err(|error| {
        ApiError::Internal(InternalError::Serialize {
            ty: std::any::type_name::<CreateChatCompletionResponse>(),
            error,
        })
    })?;
    Ok(Bytes::from(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_emits_role_then_text_then_done() {
        let mut st = BridgeStreamState::default();
        let mut acc = BytesMut::new();

        let o1 = st
            .process_upstream_sse_json(
                br#"{"type":"response.created","response":{"id":"resp_1","model":"gpt-5"}}"#,
            )
            .unwrap();
        assert!(o1.is_none());

        let o2 = st
            .process_upstream_sse_json(
                br#"{"type":"response.output_text.delta","delta":"hi"}"#,
            )
            .unwrap()
            .expect("chunk");
        acc.put(o2.as_ref());

        let o3 = st
            .process_upstream_sse_json(
                br#"{"type":"response.completed","response":{"id":"resp_1","model":"gpt-5","usage":{"input_tokens":1,"output_tokens":2,"total_tokens":3}}}"#,
            )
            .unwrap()
            .expect("done");
        acc.put(o3.as_ref());

        let s = String::from_utf8_lossy(&acc);
        assert!(s.contains("chat.completion.chunk"));
        assert!(s.contains("\"role\":\"assistant\""));
        assert!(s.contains("\"content\":\"hi\""));
        assert!(s.contains("prompt_tokens"));
        assert!(s.contains("data: [DONE]"));
    }

    #[test]
    fn non_stream_maps_basic_response() {
        let json = br#"{"id":"resp_x","created_at":1,"model":"gpt-5","object":"response","output":[],"status":"completed"}"#;
        let out = non_stream_responses_body_to_chat_completion(json).unwrap();
        let v: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["id"], "resp_x");
    }
}

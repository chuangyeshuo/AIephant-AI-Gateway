use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Clone)]
pub struct OpenAiEmbedderClient {
    pub client: reqwest::Client,
}

impl OpenAiEmbedderClient {
    #[must_use]
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    pub async fn embed(
        &self,
        base_url: &str,
        api_key: &str,
        model: &str,
        input: &str,
    ) -> Result<Vec<f32>, String> {
        let url = format!("{}/v1/embeddings", base_url.trim_end_matches('/'));
        let request_body = json!({
            "model": model,
            "input": input
        });
        tracing::info!(
            target: "semantic_cache::embedder_client",
            url = %url,
            model = %model,
            request_body = %request_body,
            "requesting embeddings model"
        );
        let resp = self
            .client
            .post(&url)
            .bearer_auth(api_key)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = resp.status();
        let response_body = resp.text().await.map_err(|e| e.to_string())?;
        tracing::info!(
            target: "semantic_cache::embedder_client",
            url = %url,
            status = %status,
            "embeddings response received"
        );
        if !status.is_success() {
            return Err(format!("openai embeddings failed: {status}"));
        }
        let parsed: EmbeddingResponse =
            serde_json::from_str(&response_body).map_err(|e| e.to_string())?;
        let Some(first) = parsed.data.into_iter().next() else {
            return Err("openai embeddings response has no vectors".to_string());
        };
        Ok(first.embedding)
    }
}

#[derive(Debug, Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingItem>,
}

#[derive(Debug, Deserialize)]
struct EmbeddingItem {
    embedding: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::OpenAiEmbedderClient;

    #[tokio::test]
    async fn embed_returns_error_on_bad_base_url() {
        let c = OpenAiEmbedderClient::new(reqwest::Client::new());
        let err = c
            .embed(
                "http://127.0.0.1:1",
                "sk-test",
                "text-embedding-3-large",
                "hello",
            )
            .await
            .unwrap_err();
        assert!(!err.is_empty());
    }
}

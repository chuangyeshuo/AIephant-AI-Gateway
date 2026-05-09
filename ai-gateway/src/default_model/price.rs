//! Per-million-token prices in `provider_models.info` JSON (matches catalog
//! design).

#[must_use]
pub fn price_sum_from_info(info: &serde_json::Value) -> f64 {
    let p = info
        .get("prompt")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    let c = info
        .get("completion")
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0);
    p + c
}

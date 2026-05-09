/// Decides whether a request model is permitted for a virtual key.
///
/// Priority (strict):
/// 1. `allowed_models` hit (case-insensitive) → allow
/// 2. `blocked_models` hit (case-insensitive) → deny
/// 3. neither list matches → allow
///
/// `None` or empty list means "no entries on that side".
#[must_use]
pub fn model_access_allowed(
    request_model: &str,
    allowed_models: Option<&[String]>,
    blocked_models: Option<&[String]>,
) -> bool {
    let req_lower = request_model.to_lowercase();

    if let Some(list) = allowed_models
        && list.iter().any(|m| m.to_lowercase() == req_lower)
    {
        return true;
    }

    if let Some(list) = blocked_models
        && list.iter().any(|m| m.to_lowercase() == req_lower)
    {
        return false;
    }

    true
}

use ai_gateway::{
    error::{api::ApiError, invalid_req::InvalidRequestError},
    types::extensions::VkPolicy,
    virtual_key::enforce::check_model_access,
};
use uuid::Uuid;

fn extensions_with_policy(
    allowed: Option<Vec<String>>,
    blocked: Option<Vec<String>>,
) -> http::Extensions {
    let mut ext = http::Extensions::new();
    ext.insert(VkPolicy {
        virtual_key_id: Uuid::new_v4(),
        allowed_models: allowed,
        blocked_models: blocked,
    });
    ext
}

#[test]
fn no_policy_always_allows() {
    let ext = http::Extensions::new();
    assert!(check_model_access(&ext, "gpt-4").is_ok());
}

#[test]
fn allowed_list_permits_exact_match() {
    let ext = extensions_with_policy(Some(vec!["gpt-4".to_string()]), None);
    assert!(check_model_access(&ext, "gpt-4").is_ok());
    assert!(check_model_access(&ext, "GPT-4").is_ok());
}

#[test]
fn allowed_list_does_not_block_unlisted_model() {
    let ext = extensions_with_policy(Some(vec!["gpt-4".to_string()]), None);
    assert!(check_model_access(&ext, "claude-3").is_ok());
}

#[test]
fn blocked_list_rejects_blocked_model() {
    let ext = extensions_with_policy(None, Some(vec!["gpt-4".to_string()]));
    let err = check_model_access(&ext, "gpt-4").unwrap_err();
    assert!(matches!(
        err,
        ApiError::InvalidRequest(InvalidRequestError::ModelAccessDenied(_))
    ));
}

#[test]
fn blocked_list_allows_other_models() {
    let ext = extensions_with_policy(None, Some(vec!["gpt-4".to_string()]));
    assert!(check_model_access(&ext, "claude-3").is_ok());
}

#[test]
fn blocked_list_rejects_unparseable_model_string() {
    let ext = extensions_with_policy(None, Some(vec!["%%%".to_string()]));
    let err = check_model_access(&ext, "%%%").unwrap_err();
    assert!(matches!(
        err,
        ApiError::InvalidRequest(InvalidRequestError::ModelAccessDenied(_))
    ));
}

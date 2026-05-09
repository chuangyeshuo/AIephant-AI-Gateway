#![allow(clippy::large_futures)]

use std::collections::HashMap;

use ai_gateway::{
    config::{Config, alephant::AlephantFeatures},
    tests::{TestDefault, harness::Harness, mock::MockArgs},
};
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::json;
use tower::Service;

#[tokio::test]
#[serial_test::serial]
async fn request_response_logger_authenticated() {
    let mut config = Config::test_default();
    // Ensure auth is required for this test
    config.alephant.features = AlephantFeatures::All;

    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:openai:chat_completion", 1.into()),
            ("success:s3:upload_request", 1.into()),
            ("success:alephant:log_request", 1.into()),
        ]))
        .build();
    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    let body_bytes = serde_json::to_vec(&json!({
        "model": "openai/gpt-4o-mini",
        "messages": [
            {
                "role": "user",
                "content": "Hello, world!"
            }
        ]
    }))
    .unwrap();

    let request_body = axum_core::body::Body::from(body_bytes.clone());
    let request = Request::builder()
        .method(Method::POST)
        .header("authorization", "Bearer sk-alephant-test-key")
        .uri("http://router.alephant.test/router/my-router/chat/completions")
        .body(request_body)
        .unwrap();
    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // we need to collect the body here in order to poll the underlying body
    // so that the async logging task can complete
    let _response_body = response.into_body().collect().await.unwrap();

    // sleep so that the background task for logging can complete
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
}

#[tokio::test]
#[serial_test::serial]
async fn authenticated_cloud_with_explicit_s3_port() {
    let mut config = Config::test_default();
    let s3_port = 9190;
    // Ensure auth is required for this test
    config.alephant.features = AlephantFeatures::All;

    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:openai:chat_completion", 1.into()),
            ("success:s3:upload_request", 1.into()),
            ("success:alephant:log_request", 1.into()),
        ]))
        .s3_port(s3_port)
        .build();
    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    let body_bytes = serde_json::to_vec(&json!({
        "model": "openai/gpt-4o-mini",
        "messages": [
            {
                "role": "user",
                "content": "Hello, world!"
            }
        ]
    }))
    .unwrap();

    let request_body = axum_core::body::Body::from(body_bytes.clone());
    let request = Request::builder()
        .method(Method::POST)
        .header("authorization", "Bearer sk-alephant-test-key")
        .uri("http://router.alephant.test/router/my-router/chat/completions")
        .body(request_body)
        .unwrap();
    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // we need to collect the body here in order to poll the underlying body
    // so that the async logging task can complete
    let _response_body = response.into_body().collect().await.unwrap();

    // sleep so that the background task for logging can complete
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
}

#[tokio::test]
#[serial_test::serial]
async fn unauthenticated_cloud_requires_auth() {
    let mut config = Config::test_default();
    let s3_port = 9190;
    // Ensure auth is required for this test
    config.alephant.features = AlephantFeatures::Auth;

    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:openai:chat_completion", 0.into()),
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .s3_port(s3_port)
        .build();
    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .with_mock_auth()
        .build()
        .await;
    let body_bytes = serde_json::to_vec(&json!({
        "model": "openai/gpt-4o-mini",
        "messages": [
            {
                "role": "user",
                "content": "Hello, world!"
            }
        ]
    }))
    .unwrap();

    let request_body = axum_core::body::Body::from(body_bytes.clone());
    let request = Request::builder()
        .method(Method::POST)
        .uri("http://router.alephant.test/router/my-router/chat/completions")
        .body(request_body)
        .unwrap();
    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial_test::serial]
async fn request_response_logger_unauthenticated() {
    let mut config = Config::test_default();
    // Disable auth requirement for this test
    config.alephant.features = AlephantFeatures::None;

    let mock_args = MockArgs::builder()
        .stubs(HashMap::from([
            ("success:openai:chat_completion", 1.into()),
            // When unauthenticated, logging services should NOT be called
            ("success:s3:upload_request", 0.into()),
            ("success:alephant:log_request", 0.into()),
        ]))
        .build();
    let mut harness = Harness::builder()
        .with_config(config)
        .with_mock_args(mock_args)
        .build()
        .await;
    let body_bytes = serde_json::to_vec(&json!({
        "model": "openai/gpt-4o-mini",
        "messages": [
            {
                "role": "user",
                "content": "Hello, world!"
            }
        ]
    }))
    .unwrap();

    let request_body = axum_core::body::Body::from(body_bytes.clone());
    let request = Request::builder()
        .method(Method::POST)
        // No authorization header when auth is not required
        .uri("http://router.alephant.test/router/my-router/chat/completions")
        .body(request_body)
        .unwrap();
    let response = harness.call(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

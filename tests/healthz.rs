use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use tower::ServiceExt;

#[tokio::test]
async fn healthz_returns_200_with_status_payload() {
    let app = music_api::app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let status = json["status"].as_str().expect("status field present");
    assert!(
        matches!(status, "ok" | "degraded" | "needs_reauth"),
        "status must be one of ok|degraded|needs_reauth, got {status:?}",
    );

    assert!(
        json["version"].is_string(),
        "version must be a string, got {:?}",
        json["version"],
    );
    assert!(
        json["token_state"].is_string(),
        "token_state must be a string, got {:?}",
        json["token_state"],
    );
    assert!(
        json["last_fetch_ts"].is_null() || json["last_fetch_ts"].is_string(),
        "last_fetch_ts must be null or ISO8601 string, got {:?}",
        json["last_fetch_ts"],
    );
}

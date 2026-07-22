use axum::{
    body::Body,
    http::{HeaderValue, Request, header},
};

use super::support::*;

#[tokio::test]
async fn request_ids_are_generated_preserved_and_replaced_when_invalid() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
        .unwrap();

    let generated = router(pool.clone(), test_auth())
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let generated_id = generated.headers()["x-request-id"].to_str().unwrap();
    assert!(Uuid::parse_str(generated_id).is_ok());

    let preserved = router(pool.clone(), test_auth())
        .oneshot(
            Request::get("/health")
                .header("x-request-id", "request-27")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(preserved.headers()["x-request-id"], "request-27");

    for value in ["contains space", "x".repeat(129).as_str()] {
        let response = router(pool.clone(), test_auth())
            .oneshot(
                Request::get("/health")
                    .header("x-request-id", value)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(Uuid::parse_str(response.headers()["x-request-id"].to_str().unwrap()).is_ok());
    }

    let mut duplicated = Request::get("/health").body(Body::empty()).unwrap();
    duplicated
        .headers_mut()
        .append("x-request-id", HeaderValue::from_static("first"));
    duplicated
        .headers_mut()
        .append("x-request-id", HeaderValue::from_static("second"));
    let response = router(pool, test_auth()).oneshot(duplicated).await.unwrap();
    assert!(Uuid::parse_str(response.headers()["x-request-id"].to_str().unwrap()).is_ok());
}

#[tokio::test]
async fn request_ids_cover_errors_and_unmatched_routes() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
        .unwrap();
    for path in ["/accounts/not-a-uuid/balance", "/not-found"] {
        let response = router(pool.clone(), test_auth())
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(response.headers().contains_key("x-request-id"));
    }
}

#[tokio::test]
async fn metrics_use_bounded_route_labels_and_prometheus_text() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
        .unwrap();
    let resource_id = Uuid::new_v4();
    let app = router(pool, test_auth());
    let _ = app
        .clone()
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let _ = app
        .clone()
        .oneshot(
            Request::get(format!("/accounts/{resource_id}/balance"))
                .header(header::AUTHORIZATION, "Bearer secret-value")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let _ = app
        .clone()
        .oneshot(Request::get("/unknown-route").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let response = app
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/plain; version=0.0.4; charset=utf-8"
    );
    let metrics = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(metrics.contains("ledger_http_requests_total"));
    assert!(metrics.contains("ledger_http_request_duration_seconds"));
    assert!(metrics.contains("route=\"/accounts/{account_id}/balance\""));
    assert!(metrics.contains("route=\"unmatched\""));
    assert!(!metrics.contains(&resource_id.to_string()));
    assert!(!metrics.contains("secret-value"));
}

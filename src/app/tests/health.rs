use super::support::*;

#[tokio::test]
async fn health_is_available_without_a_database_connection() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
        .unwrap();
    let response = router(pool, test_auth())
        .oneshot(Request::get("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), br#"{"status":"ok"}"#);
}

#[tokio::test]
async fn readiness_hides_database_failures() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://user:password@localhost/ledger")
        .unwrap();
    pool.close().await;
    let response = router(pool, test_auth())
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["code"], "service_unavailable");
    assert_eq!(
        body["error"]["message"],
        "Service is temporarily unavailable"
    );
    let body = body.to_string();
    for forbidden in ["postgres://", "user", "password", "sqlx", "PoolClosed"] {
        assert!(!body.contains(forbidden), "response exposed {forbidden}");
    }
}

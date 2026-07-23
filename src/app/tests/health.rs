use super::support::*;

fn metric_value(name: &str, labels: &[&str]) -> f64 {
    crate::observability::metrics_handle()
        .render()
        .lines()
        .find(|line| line.starts_with(name) && labels.iter().all(|label| line.contains(label)))
        .and_then(|line| line.rsplit_once(' '))
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0.0)
}

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
    let checks_before = metric_value(
        "ledger_readiness_checks_total",
        &["outcome=\"not_ready\"", "reason=\"database_unavailable\""],
    );
    let duration_before = metric_value(
        "ledger_readiness_check_duration_seconds_count",
        &["outcome=\"not_ready\""],
    );
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
    assert_eq!(
        metric_value(
            "ledger_readiness_checks_total",
            &["outcome=\"not_ready\"", "reason=\"database_unavailable\""],
        ),
        checks_before + 1.0
    );
    assert_eq!(
        metric_value(
            "ledger_readiness_check_duration_seconds_count",
            &["outcome=\"not_ready\""],
        ),
        duration_before + 1.0
    );
}

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use rust_backend_technical_assessment::{
    app,
    auth::AuthVerifier,
    config::{AuthConfig, PoolConfig},
    db,
};
use std::time::Duration;
use tower::ServiceExt;

fn metric_value(name: &str, labels: &[&str]) -> f64 {
    rust_backend_technical_assessment::observability::metrics_handle()
        .render()
        .lines()
        .find(|line| line.starts_with(name) && labels.iter().all(|label| line.contains(label)))
        .and_then(|line| line.rsplit_once(' '))
        .and_then(|(_, value)| value.parse().ok())
        .unwrap_or(0.0)
}

#[tokio::test]
async fn readiness_succeeds_against_configured_postgres() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        eprintln!("skipping PostgreSQL readiness integration test: DATABASE_URL is not set");
        return;
    };
    let pool = db::create_pool(
        &database_url,
        &PoolConfig {
            max_connections: 1,
            min_connections: 0,
            acquire_timeout: Duration::from_secs(2),
            connect_timeout: Duration::from_secs(2),
        },
    )
    .await
    .expect("DATABASE_URL must point to reachable PostgreSQL for this integration test");
    let auth = AuthVerifier::new(
        &AuthConfig::new(
            "https://issuer.example",
            "ledger",
            include_str!("fixtures/jwt-test-public.pem"),
        )
        .unwrap(),
    )
    .unwrap();
    let checks_before = metric_value(
        "ledger_readiness_checks_total",
        &["outcome=\"ready\"", "reason=\"none\""],
    );
    let duration_before = metric_value(
        "ledger_readiness_check_duration_seconds_count",
        &["outcome=\"ready\""],
    );
    let response = app::router(pool, auth)
        .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), br#"{"status":"ready"}"#);
    assert_eq!(
        metric_value(
            "ledger_readiness_checks_total",
            &["outcome=\"ready\"", "reason=\"none\""],
        ),
        checks_before + 1.0
    );
    assert_eq!(
        metric_value(
            "ledger_readiness_check_duration_seconds_count",
            &["outcome=\"ready\""],
        ),
        duration_before + 1.0
    );
}

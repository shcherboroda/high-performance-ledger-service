use axum::{
    body::Body,
    http::{HeaderValue, Request, header},
};
use std::time::Duration;

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

#[tokio::test]
async fn custom_http_methods_use_the_other_metric_label() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
        .unwrap();
    let custom_method = "CUSTOM-METHOD-27";
    let app = router(pool, test_auth());
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method(custom_method)
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let response = app
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let metrics = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();

    assert!(metrics.contains("method=\"OTHER\""));
    assert!(!metrics.contains(custom_method));
}

#[sqlx::test]
async fn financial_handlers_emit_bounded_operation_and_idempotency_metrics(pool: PgPool) {
    let owner = token(
        Some("metric-owner"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let recipient = token(
        Some("metric-recipient"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let source = Uuid::new_v4();
    let destination = Uuid::new_v4();
    insert_test_account(&pool, source, "metric-owner", "USD", 2, 10_000).await;
    insert_test_account(&pool, destination, "metric-recipient", "USD", 2, 0).await;

    let success = [
        "operation=\"transfer\"",
        "outcome=\"success\"",
        "reason=\"none\"",
    ];
    let owner_label = ["operation=\"transfer\"", "outcome=\"owner\""];
    let replay_label = ["operation=\"transfer\"", "outcome=\"replay\""];
    let conflict_label = ["operation=\"transfer\"", "outcome=\"conflict\""];
    let success_before = metric_value("ledger_operations_total", &success);
    let success_duration_before = metric_value(
        "ledger_database_transaction_duration_seconds_count",
        &["operation=\"transfer\"", "outcome=\"success\""],
    );
    let owner_before = metric_value("ledger_idempotency_outcomes_total", &owner_label);
    let replay_before = metric_value("ledger_idempotency_outcomes_total", &replay_label);
    let conflict_before = metric_value("ledger_idempotency_outcomes_total", &conflict_label);
    let rejection = [
        "operation=\"transfer\"",
        "outcome=\"rejected\"",
        "reason=\"idempotency_conflict\"",
    ];
    let rejection_before = metric_value("ledger_operations_total", &rejection);

    let request = json!({"source_account_id": source, "destination_account_id": destination, "amount": "10.00"});
    let first = transfer_response(pool.clone(), &owner, "metrics-transfer", request.clone()).await;
    assert_eq!(first.status(), StatusCode::CREATED);
    assert_eq!(
        metric_value("ledger_operations_total", &success),
        success_before + 1.0
    );
    assert_eq!(
        metric_value(
            "ledger_database_transaction_duration_seconds_count",
            &["operation=\"transfer\"", "outcome=\"success\""],
        ),
        success_duration_before + 1.0
    );
    assert_eq!(
        metric_value("ledger_idempotency_outcomes_total", &owner_label),
        owner_before + 1.0
    );

    assert_eq!(
        transfer_response(pool.clone(), &owner, "metrics-transfer", request.clone())
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        metric_value("ledger_idempotency_outcomes_total", &replay_label),
        replay_before + 1.0
    );
    assert_eq!(
        metric_value("ledger_operations_total", &success),
        success_before + 2.0
    );

    assert_eq!(transfer_response(pool.clone(), &owner, "metrics-transfer", json!({"source_account_id": source, "destination_account_id": destination, "amount": "10.01"})).await.status(), StatusCode::CONFLICT);
    assert_eq!(
        metric_value("ledger_idempotency_outcomes_total", &conflict_label),
        conflict_before + 1.0
    );
    assert_eq!(
        metric_value("ledger_operations_total", &rejection),
        rejection_before + 1.0
    );

    let insufficient = [
        "operation=\"transfer\"",
        "outcome=\"rejected\"",
        "reason=\"insufficient_funds\"",
    ];
    let insufficient_before = metric_value("ledger_operations_total", &insufficient);
    let rejected_duration_before = metric_value(
        "ledger_database_transaction_duration_seconds_count",
        &["operation=\"transfer\"", "outcome=\"rejected\""],
    );
    assert_eq!(transfer_response(pool.clone(), &owner, "metrics-rejected-owner", json!({"source_account_id": source, "destination_account_id": destination, "amount": "1000.00"})).await.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        metric_value("ledger_operations_total", &insufficient),
        insufficient_before + 1.0
    );
    assert_eq!(
        metric_value(
            "ledger_database_transaction_duration_seconds_count",
            &["operation=\"transfer\"", "outcome=\"rejected\""],
        ),
        rejected_duration_before + 1.0
    );

    let fx_source = Uuid::new_v4();
    let fx_destination = Uuid::new_v4();
    insert_test_account(&pool, fx_source, "metric-owner", "EUR", 2, 10_000).await;
    insert_test_account(&pool, fx_destination, "metric-recipient", "PLN", 2, 0).await;
    sqlx::query("INSERT INTO exchange_rates (id, source_currency, destination_currency, rate, valid_from, valid_until) VALUES ($1, 'EUR', 'PLN', 4, now() - interval '1 hour', now() + interval '1 hour')")
        .bind(Uuid::new_v4()).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO fx_fee_rules (id, fee_bps, valid_from, valid_until) VALUES ($1, 0, now() - interval '1 hour', now() + interval '1 hour')")
        .bind(Uuid::new_v4()).execute(&pool).await.unwrap();
    let fx_success = [
        "operation=\"fx_transfer\"",
        "outcome=\"success\"",
        "reason=\"none\"",
    ];
    let fx_before = metric_value("ledger_operations_total", &fx_success);
    assert_eq!(transfer_response(pool.clone(), &owner, "metrics-fx", json!({"source_account_id": fx_source, "destination_account_id": fx_destination, "amount": "10.00"})).await.status(), StatusCode::CREATED);
    assert_eq!(
        metric_value("ledger_operations_total", &fx_success),
        fx_before + 1.0
    );

    let reversal_success = [
        "operation=\"reversal\"",
        "outcome=\"success\"",
        "reason=\"none\"",
    ];
    let reversal_before = metric_value("ledger_operations_total", &reversal_success);
    let reversal_replay = ["operation=\"reversal\"", "outcome=\"replay\""];
    let reversal_replay_before =
        metric_value("ledger_idempotency_outcomes_total", &reversal_replay);
    let transfer_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM transfers WHERE source_account_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(source)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        reversal_response(pool.clone(), &recipient, "metrics-reversal", transfer_id)
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        metric_value("ledger_operations_total", &reversal_success),
        reversal_before + 1.0
    );
    assert_eq!(
        reversal_response(pool.clone(), &recipient, "metrics-reversal", transfer_id)
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        metric_value("ledger_idempotency_outcomes_total", &reversal_replay),
        reversal_replay_before + 1.0
    );

    let internal_duration_before = metric_value(
        "ledger_database_transaction_duration_seconds_count",
        &["operation=\"transfer\"", "outcome=\"internal_error\""],
    );
    sqlx::query("CREATE FUNCTION reject_metric_entry() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'forced metric failure'; END; $$")
        .execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_metric_entry BEFORE INSERT ON account_entries FOR EACH ROW EXECUTE FUNCTION reject_metric_entry()")
        .execute(&pool).await.unwrap();
    assert_eq!(transfer_response(pool.clone(), &owner, "metrics-internal", json!({"source_account_id": source, "destination_account_id": destination, "amount": "1.00"})).await.status(), StatusCode::INTERNAL_SERVER_ERROR);
    sqlx::query("DROP TRIGGER reject_metric_entry ON account_entries")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP FUNCTION reject_metric_entry()")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        metric_value(
            "ledger_database_transaction_duration_seconds_count",
            &["operation=\"transfer\"", "outcome=\"internal_error\""],
        ),
        internal_duration_before + 1.0
    );

    let reversal_internal_duration_before = metric_value(
        "ledger_database_transaction_duration_seconds_count",
        &["operation=\"reversal\"", "outcome=\"internal_error\""],
    );
    let unavailable_pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(100))
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
        .unwrap();
    assert_eq!(
        reversal_response(
            unavailable_pool,
            &recipient,
            "metrics-begin-failure",
            Uuid::new_v4()
        )
        .await
        .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        metric_value(
            "ledger_database_transaction_duration_seconds_count",
            &["operation=\"reversal\"", "outcome=\"internal_error\""],
        ),
        reversal_internal_duration_before
    );
}

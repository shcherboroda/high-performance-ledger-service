use super::support::*;

async fn get(pool: PgPool, token: &str, uri: &str) -> axum::response::Response {
    account_response(pool, "GET", uri, token, None).await
}

#[sqlx::test]
async fn history_paginates_owned_entries_and_binds_its_cursor(pool: PgPool) {
    let account = Uuid::new_v4();
    let counterparty = Uuid::new_v4();
    insert_test_account(&pool, account, "client-123", "USD", 2, 0).await;
    insert_test_account(&pool, counterparty, "client-456", "USD", 2, 0).await;
    for second in 1..=3 {
        let transfer = Uuid::new_v4();
        sqlx::query("INSERT INTO transfers (id, source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, total_source_debit_minor, kind, initiated_by, created_at) VALUES ($1, $2, $3, 'USD', 'USD', 100, 100, 100, 'transfer', 'client-123', $4::timestamptz)")
            .bind(transfer).bind(account).bind(counterparty).bind(format!("2026-01-01 00:00:0{second}+00")).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO account_entries (id, account_id, transfer_id, counterparty_account_id, direction, operation_kind, amount_minor, currency, created_at) VALUES ($1, $2, $3, $4, 'debit', 'transfer', 100, 'USD', $5::timestamptz)")
            .bind(Uuid::new_v4()).bind(account).bind(transfer).bind(counterparty).bind(format!("2026-01-01 00:00:0{second}+00")).execute(&pool).await.unwrap();
    }
    let token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let first = get(
        pool.clone(),
        &token,
        &format!("/accounts/{account}/entries?limit=2"),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let first: Value =
        serde_json::from_slice(&to_bytes(first.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    assert_eq!(first["items"][0]["amount"], "1.00");
    let cursor = first["next_cursor"].as_str().unwrap();
    let second = get(
        pool.clone(),
        &token,
        &format!("/accounts/{account}/entries?limit=2&cursor={cursor}"),
    )
    .await;
    let second: Value =
        serde_json::from_slice(&to_bytes(second.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert!(second["next_cursor"].is_null());
    let invalid = get(
        pool.clone(),
        &token,
        &format!("/accounts/{account}/entries?cursor=v1.00"),
    )
    .await;
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
    let foreign = get(pool, &token, &format!("/accounts/{counterparty}/entries")).await;
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn history_filters_validates_and_orders_tied_entries(pool: PgPool) {
    let account = Uuid::new_v4();
    let counterparty = Uuid::new_v4();
    let other_counterparty = Uuid::new_v4();
    insert_test_account(&pool, account, "client-123", "JPY", 0, 0).await;
    insert_test_account(&pool, counterparty, "client-456", "JPY", 0, 0).await;
    insert_test_account(&pool, other_counterparty, "client-789", "JPY", 0, 0).await;
    let timestamp = "2026-02-03 04:05:06+00";
    let first_transfer = Uuid::new_v4();
    let second_transfer = Uuid::new_v4();
    for (transfer, kind) in [(first_transfer, "transfer"), (second_transfer, "reversal")] {
        sqlx::query("INSERT INTO transfers (id, source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, total_source_debit_minor, kind, initiated_by, created_at) VALUES ($1, $2, $3, 'JPY', 'JPY', 5, 5, 5, $4::transfer_kind, 'client-123', $5::timestamptz)")
            .bind(transfer).bind(account).bind(counterparty).bind(kind).bind(timestamp).execute(&pool).await.unwrap();
    }
    let lower = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
    let higher = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
    for (entry_id, transfer, kind, party) in [
        (lower, first_transfer, "transfer", counterparty),
        (higher, second_transfer, "reversal", counterparty),
    ] {
        sqlx::query("INSERT INTO account_entries (id, account_id, transfer_id, counterparty_account_id, direction, operation_kind, amount_minor, currency, principal_amount_minor, fee_amount_minor, created_at) VALUES ($1, $2, $3, $4, 'credit', $5::transfer_kind, 5, 'JPY', NULL, NULL, $6::timestamptz)")
            .bind(entry_id).bind(account).bind(transfer).bind(party).bind(kind).bind(timestamp).execute(&pool).await.unwrap();
    }
    let token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let first = get(
        pool.clone(),
        &token,
        &format!("/accounts/{account}/entries?limit=1"),
    )
    .await;
    let first: Value =
        serde_json::from_slice(&to_bytes(first.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(first["items"][0]["entry_id"], higher.to_string());
    assert_eq!(first["items"][0]["amount"], "5");
    assert_eq!(first["items"][0]["operation_kind"], "reversal");
    assert!(first["items"][0]["principal_amount"].is_null());
    assert!(first["items"][0]["fee_amount"].is_null());
    let cursor = first["next_cursor"].as_str().unwrap();
    let second = get(
        pool.clone(),
        &token,
        &format!("/accounts/{account}/entries?limit=1&cursor={cursor}"),
    )
    .await;
    let second: Value =
        serde_json::from_slice(&to_bytes(second.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(second["items"][0]["entry_id"], lower.to_string());
    assert!(second["next_cursor"].is_null());
    let filtered = get(
        pool.clone(),
        &token,
        &format!("/accounts/{account}/entries?counterparty_account_id={counterparty}"),
    )
    .await;
    let filtered: Value =
        serde_json::from_slice(&to_bytes(filtered.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(filtered["items"].as_array().unwrap().len(), 2);
    let empty = get(
        pool.clone(),
        &token,
        &format!("/accounts/{account}/entries?counterparty_account_id={other_counterparty}"),
    )
    .await;
    let empty: Value =
        serde_json::from_slice(&to_bytes(empty.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert!(empty["items"].as_array().unwrap().is_empty());
    for query in ["limit=0", "limit=101", "limit=bad", "limit=1&limit=2"] {
        let response = get(
            pool.clone(),
            &token,
            &format!("/accounts/{account}/entries?{query}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
    }
    let mismatch = get(
        pool.clone(),
        &token,
        &format!(
            "/accounts/{account}/entries?counterparty_account_id={counterparty}&cursor={cursor}"
        ),
    )
    .await;
    assert_eq!(mismatch.status(), StatusCode::BAD_REQUEST);
    for cursor in ["v1.00", "v2.00"] {
        let response = get(
            pool.clone(),
            &token,
            &format!("/accounts/{account}/entries?cursor={cursor}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    let history_plan: Vec<String> = sqlx::query_scalar(
        "EXPLAIN (COSTS OFF) SELECT id FROM account_entries WHERE account_id = $1 ORDER BY created_at DESC, id DESC LIMIT 2",
    )
    .bind(account)
    .fetch_all(&pool)
    .await
    .unwrap();
    let pair_plan: Vec<String> = sqlx::query_scalar(
        "EXPLAIN (COSTS OFF) SELECT id FROM account_entries WHERE account_id = $1 AND counterparty_account_id = $2 ORDER BY created_at DESC, id DESC LIMIT 2",
    )
    .bind(account)
    .bind(counterparty)
    .fetch_all(&pool)
    .await
    .unwrap();
    for plan in [history_plan, pair_plan] {
        assert!(
            plan.iter()
                .any(|line| line.contains("account_entries_") || line.contains("Seq Scan")),
            "unexpected history query plan: {plan:?}"
        );
    }
}

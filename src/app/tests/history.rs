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

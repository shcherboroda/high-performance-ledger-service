use super::support::*;

async fn get(pool: PgPool, token: &str, uri: &str) -> axum::response::Response {
    account_response(pool, "GET", uri, token, None).await
}

fn encoded_cursor_with_timestamp(account_id: Uuid, timestamp: &str) -> String {
    let payload = json!({
        "v": 1,
        "account_id": account_id,
        "counterparty_account_id": Value::Null,
        "created_at": timestamp,
        "entry_id": Uuid::new_v4(),
    });
    let mut output = String::from("v1.");
    for byte in payload.to_string().bytes() {
        use std::fmt::Write;
        write!(output, "{byte:02x}").unwrap();
    }
    output
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
    for (entry_id, transfer, kind, direction, principal, fee) in [
        (
            lower,
            first_transfer,
            "transfer",
            "debit",
            Some(4_i64),
            Some(1_i64),
        ),
        (higher, second_transfer, "reversal", "credit", None, None),
    ] {
        sqlx::query("INSERT INTO account_entries (id, account_id, transfer_id, counterparty_account_id, direction, operation_kind, amount_minor, currency, principal_amount_minor, fee_amount_minor, created_at) VALUES ($1, $2, $3, $4, $5::entry_direction, $6::transfer_kind, 5, 'JPY', $7, $8, $9::timestamptz)")
            .bind(entry_id).bind(account).bind(transfer).bind(counterparty).bind(direction).bind(kind).bind(principal).bind(fee).bind(timestamp).execute(&pool).await.unwrap();
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
    assert_eq!(second["items"][0]["direction"], "debit");
    assert_eq!(second["items"][0]["principal_amount"], "4");
    assert_eq!(second["items"][0]["fee_amount"], "1");
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
    let duplicate_cursor = get(
        pool.clone(),
        &token,
        &format!("/accounts/{account}/entries?cursor={cursor}&cursor={cursor}"),
    )
    .await;
    assert_eq!(duplicate_cursor.status(), StatusCode::BAD_REQUEST);
    let another_account = Uuid::new_v4();
    insert_test_account(&pool, another_account, "client-123", "JPY", 0, 0).await;
    let cross_account = get(
        pool.clone(),
        &token,
        &format!("/accounts/{another_account}/entries?cursor={cursor}"),
    )
    .await;
    assert_eq!(cross_account.status(), StatusCode::BAD_REQUEST);
    let invalid_calendar = encoded_cursor_with_timestamp(account, "2026-99-99T99:99:99Z");
    let invalid_calendar = get(
        pool.clone(),
        &token,
        &format!("/accounts/{account}/entries?cursor={invalid_calendar}"),
    )
    .await;
    let invalid_calendar: Value = serde_json::from_slice(
        &to_bytes(invalid_calendar.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(invalid_calendar["error"]["code"], "invalid_cursor");

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
    assert!(!history_plan.is_empty());
    assert!(!pair_plan.is_empty());
    for (index, expected_columns) in [
        (
            "account_entries_history",
            "account_id, created_at DESC, id DESC",
        ),
        (
            "account_entries_pair_history",
            "account_id, counterparty_account_id, created_at DESC, id DESC",
        ),
    ] {
        let definition: String = sqlx::query_scalar(
            "SELECT pg_get_indexdef(indexrelid) FROM pg_index WHERE indexrelid = $1::regclass",
        )
        .bind(index)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            definition.contains(expected_columns),
            "unexpected {index}: {definition}"
        );
    }
}

#[sqlx::test]
async fn history_covers_safe_visibility_empty_inactive_and_pagination_stability(pool: PgPool) {
    let account = Uuid::new_v4();
    let counterparty = Uuid::new_v4();
    let empty = Uuid::new_v4();
    let foreign = Uuid::new_v4();
    insert_test_account(&pool, account, "client-123", "USD", 2, 0).await;
    insert_test_account(&pool, counterparty, "client-456", "USD", 2, 0).await;
    insert_test_account(&pool, empty, "client-123", "USD", 2, 0).await;
    insert_test_account(&pool, foreign, "client-456", "USD", 2, 0).await;
    let token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let empty_response = get(pool.clone(), &token, &format!("/accounts/{empty}/entries")).await;
    let empty_body: Value = serde_json::from_slice(
        &to_bytes(empty_response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(empty_body["items"].as_array().unwrap().is_empty());
    assert!(empty_body["next_cursor"].is_null());
    sqlx::query("UPDATE accounts SET status = 'inactive' WHERE id = $1")
        .bind(empty)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        get(pool.clone(), &token, &format!("/accounts/{empty}/entries"))
            .await
            .status(),
        StatusCode::OK
    );
    let missing = get(
        pool.clone(),
        &token,
        &format!("/accounts/{}/entries", Uuid::new_v4()),
    )
    .await;
    let foreign_response = get(
        pool.clone(),
        &token,
        &format!("/accounts/{foreign}/entries"),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        to_bytes(missing.into_body(), usize::MAX).await.unwrap(),
        to_bytes(foreign_response.into_body(), usize::MAX)
            .await
            .unwrap()
    );
    let malformed = get(
        pool.clone(),
        &token,
        &format!("/accounts/{account}/entries?counterparty_account_id=nope"),
    )
    .await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);

    let mut expected = Vec::new();
    for index in 0..105 {
        let transfer = Uuid::new_v4();
        let entry = Uuid::new_v4();
        let time = format!("2026-03-01 00:{:02}:{:02}+00", index / 60, index % 60);
        sqlx::query("INSERT INTO transfers (id, source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, total_source_debit_minor, kind, initiated_by, created_at) VALUES ($1, $2, $3, 'USD', 'USD', 1, 1, 1, 'transfer', 'client-123', $4::timestamptz)").bind(transfer).bind(account).bind(counterparty).bind(&time).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO account_entries (id, account_id, transfer_id, counterparty_account_id, direction, operation_kind, amount_minor, currency, created_at) VALUES ($1, $2, $3, $4, 'debit', 'transfer', 1, 'USD', $5::timestamptz)").bind(entry).bind(account).bind(transfer).bind(counterparty).bind(&time).execute(&pool).await.unwrap();
        expected.push(entry.to_string());
    }
    let default = get(
        pool.clone(),
        &token,
        &format!("/accounts/{account}/entries"),
    )
    .await;
    let default: Value =
        serde_json::from_slice(&to_bytes(default.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(default["items"].as_array().unwrap().len(), 50);
    let hundred = get(
        pool.clone(),
        &token,
        &format!("/accounts/{account}/entries?limit=100"),
    )
    .await;
    let hundred: Value =
        serde_json::from_slice(&to_bytes(hundred.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(hundred["items"].as_array().unwrap().len(), 100);
    let mut uri = format!("/accounts/{account}/entries?limit=1");
    let first = get(pool.clone(), &token, &uri).await;
    let first: Value =
        serde_json::from_slice(&to_bytes(first.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 1);
    let mut seen = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["entry_id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let cursor = first["next_cursor"].as_str().unwrap().to_owned();
    let newer_transfer = Uuid::new_v4();
    sqlx::query("INSERT INTO transfers (id, source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, total_source_debit_minor, kind, initiated_by, created_at) VALUES ($1, $2, $3, 'USD', 'USD', 1, 1, 1, 'transfer', 'client-123', '2026-04-01 00:00:00+00')").bind(newer_transfer).bind(account).bind(counterparty).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO account_entries (id, account_id, transfer_id, counterparty_account_id, direction, operation_kind, amount_minor, currency, created_at) VALUES ($1, $2, $3, $4, 'debit', 'transfer', 1, 'USD', '2026-04-01 00:00:00+00')").bind(Uuid::new_v4()).bind(account).bind(newer_transfer).bind(counterparty).execute(&pool).await.unwrap();
    uri = format!("/accounts/{account}/entries?limit=25&cursor={cursor}");
    loop {
        let response = get(pool.clone(), &token, &uri).await;
        let page: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        seen.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["entry_id"].as_str().unwrap().to_owned()),
        );
        match page["next_cursor"].as_str() {
            Some(cursor) => uri = format!("/accounts/{account}/entries?limit=25&cursor={cursor}"),
            None => break,
        }
    }
    seen.sort();
    expected.sort();
    assert_eq!(seen, expected);
}

#[tokio::test]
async fn history_database_failures_use_the_safe_error_envelope() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://user:password@127.0.0.1:1/ledger")
        .unwrap();
    let token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let response = get(
        pool,
        &token,
        &format!("/accounts/{}/entries", Uuid::new_v4()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        r#"{"error":{"code":"internal_error","message":"An internal error occurred","details":null,"request_id":null}}"#
    );
}

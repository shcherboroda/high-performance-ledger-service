use super::support::*;

#[sqlx::test]
async fn reversal_is_atomic_authorized_and_replayable(pool: PgPool) {
    let original_source = Uuid::new_v4();
    let original_destination = Uuid::new_v4();
    insert_test_account(&pool, original_source, "client-123", "USD", 2, 1_000).await;
    insert_test_account(&pool, original_destination, "client-456", "USD", 2, -100).await;
    let source_token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let destination_token = token(
        Some("client-456"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let original = transfer_response(
        pool.clone(),
        &source_token,
        "original-transfer",
        json!({"source_account_id": original_source, "destination_account_id": original_destination, "amount": "10.00"}),
    )
    .await;
    let original_body: Value =
        serde_json::from_slice(&to_bytes(original.into_body(), usize::MAX).await.unwrap()).unwrap();
    let original_id = original_body["id"].as_str().unwrap().parse().unwrap();
    sqlx::query(
        "UPDATE transfers SET fee_amount_minor = 25, fee_bps = 250, exchange_rate = 1.5 WHERE id = $1",
    )
    .bind(original_id)
    .execute(&pool)
    .await
    .unwrap();
    let original_snapshot: (i64, i64, i64, i64, String, Option<Uuid>) = sqlx::query_as(
        "SELECT source_amount_minor, destination_amount_minor, fee_amount_minor, total_source_debit_minor, kind::text, reverses_transfer_id FROM transfers WHERE id = $1",
    )
    .bind(original_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    let original_entry_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM account_entries WHERE transfer_id = $1")
            .bind(original_id)
            .fetch_one(&pool)
            .await
            .unwrap();

    let foreign =
        reversal_response(pool.clone(), &source_token, "foreign-reversal", original_id).await;
    let missing = reversal_response(
        pool.clone(),
        &source_token,
        "missing-reversal",
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        to_bytes(foreign.into_body(), usize::MAX).await.unwrap(),
        to_bytes(missing.into_body(), usize::MAX).await.unwrap()
    );

    let first = reversal_response(
        pool.clone(),
        &destination_token,
        "reversal-key",
        original_id,
    )
    .await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first_body = to_bytes(first.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&first_body).unwrap();
    assert_eq!(body["source_account_id"], original_destination.to_string());
    assert_eq!(body["destination_account_id"], original_source.to_string());
    assert_eq!(body["source_amount"], "10.00");
    assert_eq!(body["total_source_debit"], "10.00");
    assert_eq!(body["original_fee_amount"], "0.25");
    assert_eq!(body["resulting_source_balance"], "-1.00");
    assert_eq!(body["resulting_destination_balance"], "10.00");
    for id in [original_source, original_destination] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT version FROM accounts WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );
    }
    let reversal_id = body["id"].as_str().unwrap().parse::<Uuid>().unwrap();
    assert_eq!(
        sqlx::query_as::<_, (Uuid, Uuid, i64, i64, i64, i64, String)>(
            "SELECT source_account_id, destination_account_id, source_amount_minor, destination_amount_minor, fee_amount_minor, total_source_debit_minor, kind::text FROM transfers WHERE id = $1",
        )
        .bind(reversal_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        (original_destination, original_source, 1_000, 1_000, 25, 1_000, "reversal".into())
    );
    assert_eq!(
        sqlx::query_as::<_, (Option<i32>, bool)>(
            "SELECT fee_bps, exchange_rate = 1.5 FROM transfers WHERE id = $1",
        )
        .bind(reversal_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        (Some(250), true)
    );
    assert_eq!(
        sqlx::query_scalar::<_, Option<Uuid>>(
            "SELECT reverses_transfer_id FROM transfers WHERE id = $1"
        )
        .bind(reversal_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        Some(original_id)
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, i64, i64, i64, String, Option<Uuid>)>(
            "SELECT source_amount_minor, destination_amount_minor, fee_amount_minor, total_source_debit_minor, kind::text, reverses_transfer_id FROM transfers WHERE id = $1",
        )
        .bind(original_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        original_snapshot
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM account_entries WHERE transfer_id = $1")
            .bind(original_id)
            .fetch_one(&pool)
            .await
            .unwrap(),
        original_entry_count
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM transfers WHERE kind = 'reversal'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM account_entries WHERE operation_kind = 'reversal'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        2
    );
    let replay = reversal_response(
        pool.clone(),
        &destination_token,
        "reversal-key",
        original_id,
    )
    .await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(
        to_bytes(replay.into_body(), usize::MAX).await.unwrap(),
        first_body
    );
    let duplicate =
        reversal_response(pool.clone(), &destination_token, "second-key", original_id).await;
    assert_eq!(duplicate.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test]
async fn late_reversal_failure_rolls_back_every_write_and_allows_retry(pool: PgPool) {
    let source = Uuid::new_v4();
    let destination = Uuid::new_v4();
    insert_test_account(&pool, source, "client-123", "USD", 2, 500).await;
    insert_test_account(&pool, destination, "client-456", "USD", 2, 100).await;
    let source_token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let destination_token = token(
        Some("client-456"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let original = transfer_response(
        pool.clone(),
        &source_token,
        "rollback-original",
        json!({"source_account_id": source, "destination_account_id": destination, "amount": "1.00"}),
    )
    .await;
    let original: Value =
        serde_json::from_slice(&to_bytes(original.into_body(), usize::MAX).await.unwrap()).unwrap();
    let original_id: Uuid = original["id"].as_str().unwrap().parse().unwrap();
    let before_accounts: Vec<(Uuid, i64, i64)> = sqlx::query_as(
        "SELECT id, balance_minor, version FROM accounts WHERE id = ANY($1) ORDER BY id",
    )
    .bind(vec![source, destination])
    .fetch_all(&pool)
    .await
    .unwrap();
    let before_original: (i64, i64, i64, i64, String) = sqlx::query_as(
        "SELECT source_amount_minor, destination_amount_minor, fee_amount_minor, total_source_debit_minor, kind::text FROM transfers WHERE id = $1",
    )
    .bind(original_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("CREATE FUNCTION reject_reversal_entry() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'forced reversal entry failure'; END; $$")
        .execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_reversal_entry BEFORE INSERT ON account_entries FOR EACH ROW WHEN (NEW.operation_kind = 'reversal') EXECUTE FUNCTION reject_reversal_entry()")
        .execute(&pool).await.unwrap();

    let failed = reversal_response(
        pool.clone(),
        &destination_token,
        "late-reversal",
        original_id,
    )
    .await;
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let failed_body = String::from_utf8(
        to_bytes(failed.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    for forbidden in ["forced reversal", "postgres", "sqlx", "DATABASE_URL"] {
        assert!(!failed_body.contains(forbidden));
    }
    assert_eq!(
        sqlx::query_as::<_, (Uuid, i64, i64)>(
            "SELECT id, balance_minor, version FROM accounts WHERE id = ANY($1) ORDER BY id",
        )
        .bind(vec![source, destination])
        .fetch_all(&pool)
        .await
        .unwrap(),
        before_accounts
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM transfers WHERE kind = 'reversal'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM account_entries WHERE operation_kind = 'reversal'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, i64, i64, i64, String)>(
            "SELECT source_amount_minor, destination_amount_minor, fee_amount_minor, total_source_debit_minor, kind::text FROM transfers WHERE id = $1",
        )
        .bind(original_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        before_original
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM idempotency_records WHERE operation_type = 'reversal' AND idempotency_key = 'late-reversal'")
            .fetch_one(&pool).await.unwrap(),
        0
    );
    sqlx::query("DROP TRIGGER reject_reversal_entry ON account_entries")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP FUNCTION reject_reversal_entry()")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        reversal_response(pool, &destination_token, "late-reversal", original_id)
            .await
            .status(),
        StatusCode::CREATED
    );
}

#[sqlx::test]
async fn concurrent_reversals_are_serialized_by_transfer_and_idempotency_keys(pool: PgPool) {
    let first_source = Uuid::new_v4();
    let first_destination = Uuid::new_v4();
    let second_source = Uuid::new_v4();
    let second_destination = Uuid::new_v4();
    let third_source = Uuid::new_v4();
    let third_destination = Uuid::new_v4();
    let fourth_source = Uuid::new_v4();
    let fourth_destination = Uuid::new_v4();
    for (id, owner) in [
        (first_source, "client-123"),
        (second_source, "client-123"),
        (third_source, "client-123"),
        (fourth_source, "client-123"),
        (first_destination, "client-456"),
        (second_destination, "client-456"),
        (third_destination, "client-456"),
        (fourth_destination, "client-456"),
    ] {
        insert_test_account(&pool, id, owner, "USD", 2, 1_000).await;
    }
    let source_token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let destination_token = token(
        Some("client-456"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let first_original = transfer_response(pool.clone(), &source_token, "first-original", json!({"source_account_id": first_source, "destination_account_id": first_destination, "amount": "1.00"})).await;
    let first_id: Uuid = serde_json::from_slice::<Value>(
        &to_bytes(first_original.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let second_original = transfer_response(pool.clone(), &source_token, "second-original", json!({"source_account_id": second_source, "destination_account_id": second_destination, "amount": "1.00"})).await;
    let second_id: Uuid = serde_json::from_slice::<Value>(
        &to_bytes(second_original.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    let (first, second) = tokio::join!(
        reversal_response(pool.clone(), &destination_token, "race-a", first_id),
        reversal_response(pool.clone(), &destination_token, "race-b", first_id),
    );
    assert!(
        (first.status() == StatusCode::CREATED
            && second.status() == StatusCode::UNPROCESSABLE_ENTITY)
            || (first.status() == StatusCode::UNPROCESSABLE_ENTITY
                && second.status() == StatusCode::CREATED)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM transfers WHERE reverses_transfer_id = $1"
        )
        .bind(first_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );

    let (same_key_first, same_key_second) = tokio::join!(
        reversal_response(pool.clone(), &destination_token, "same-key", second_id),
        reversal_response(pool.clone(), &destination_token, "same-key", second_id),
    );
    assert_eq!(same_key_first.status(), StatusCode::CREATED);
    assert_eq!(same_key_second.status(), StatusCode::CREATED);
    assert_eq!(
        to_bytes(same_key_first.into_body(), usize::MAX)
            .await
            .unwrap(),
        to_bytes(same_key_second.into_body(), usize::MAX)
            .await
            .unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM transfers WHERE reverses_transfer_id = $1"
        )
        .bind(second_id)
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        reversal_response(pool.clone(), &destination_token, "same-key", first_id)
            .await
            .status(),
        StatusCode::CONFLICT
    );

    let third_original = transfer_response(pool.clone(), &source_token, "third-original", json!({"source_account_id": third_source, "destination_account_id": third_destination, "amount": "1.00"})).await;
    let third_id: Uuid = serde_json::from_slice::<Value>(
        &to_bytes(third_original.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let fourth_original = transfer_response(pool.clone(), &source_token, "fourth-original", json!({"source_account_id": fourth_source, "destination_account_id": fourth_destination, "amount": "1.00"})).await;
    let fourth_id: Uuid = serde_json::from_slice::<Value>(
        &to_bytes(fourth_original.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let (winner, conflict) = tokio::join!(
        reversal_response(
            pool.clone(),
            &destination_token,
            "same-different-key",
            third_id
        ),
        reversal_response(
            pool.clone(),
            &destination_token,
            "same-different-key",
            fourth_id
        ),
    );
    assert!(
        (winner.status() == StatusCode::CREATED && conflict.status() == StatusCode::CONFLICT)
            || (winner.status() == StatusCode::CONFLICT
                && conflict.status() == StatusCode::CREATED)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM transfers WHERE reverses_transfer_id = ANY($1)"
        )
        .bind(vec![third_id, fourth_id])
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
}

#[sqlx::test]
async fn reversal_and_normal_transfer_serialize_without_deadlock(pool: PgPool) {
    let source = Uuid::new_v4();
    let destination = Uuid::new_v4();
    insert_test_account(&pool, source, "client-123", "USD", 2, 1_000).await;
    insert_test_account(&pool, destination, "client-456", "USD", 2, 1_000).await;
    let source_token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let destination_token = token(
        Some("client-456"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let original = transfer_response(pool.clone(), &source_token, "interaction-original", json!({"source_account_id": source, "destination_account_id": destination, "amount": "1.00"})).await;
    let original_id: Uuid =
        serde_json::from_slice::<Value>(&to_bytes(original.into_body(), usize::MAX).await.unwrap())
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
    let (reversal, transfer) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(
            reversal_response(pool.clone(), &destination_token, "interaction-reversal", original_id),
            transfer_response(pool.clone(), &destination_token, "interaction-transfer", json!({"source_account_id": destination, "destination_account_id": source, "amount": "0.25"})),
        )
    })
    .await
    .expect("reversal and transfer must not deadlock");
    assert_eq!(reversal.status(), StatusCode::CREATED);
    assert_eq!(transfer.status(), StatusCode::CREATED);
    assert_eq!(
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT balance_minor, version FROM accounts WHERE id = $1",
        )
        .bind(source)
        .fetch_one(&pool)
        .await
        .unwrap(),
        (1_025, 3)
    );
    assert_eq!(
        sqlx::query_as::<_, (i64, i64)>(
            "SELECT balance_minor, version FROM accounts WHERE id = $1",
        )
        .bind(destination)
        .fetch_one(&pool)
        .await
        .unwrap(),
        (975, 3)
    );
}

#[sqlx::test]
async fn reversal_validates_ids_and_rejects_inactive_accounts_and_reversal_targets(pool: PgPool) {
    let source = Uuid::new_v4();
    let destination = Uuid::new_v4();
    insert_test_account(&pool, source, "client-123", "USD", 2, 500).await;
    insert_test_account(&pool, destination, "client-456", "USD", 2, 0).await;
    let source_token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let destination_token = token(
        Some("client-456"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let malformed = account_response_with_key(
        pool.clone(),
        "POST",
        "/transfers/not-a-uuid/reversal",
        &destination_token,
        "malformed-reversal",
        None,
    )
    .await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        serde_json::from_slice::<Value>(
            &to_bytes(malformed.into_body(), usize::MAX).await.unwrap()
        )
        .unwrap()["error"]["code"],
        "malformed_transfer_id"
    );
    let original = transfer_response(pool.clone(), &source_token, "status-original", json!({"source_account_id": source, "destination_account_id": destination, "amount": "1.00"})).await;
    let original_id: Uuid =
        serde_json::from_slice::<Value>(&to_bytes(original.into_body(), usize::MAX).await.unwrap())
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
    for (id, key) in [
        (source, "inactive-source"),
        (destination, "inactive-destination"),
    ] {
        sqlx::query("UPDATE accounts SET status = 'inactive' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        let response = reversal_response(pool.clone(), &destination_token, key, original_id).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            serde_json::from_slice::<Value>(
                &to_bytes(response.into_body(), usize::MAX).await.unwrap()
            )
            .unwrap()["error"]["code"],
            "account_unavailable"
        );
        assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM idempotency_records WHERE operation_type = 'reversal' AND idempotency_key = $1").bind(key).fetch_one(&pool).await.unwrap(), 0);
        sqlx::query("UPDATE accounts SET status = 'active' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
    let reversal = reversal_response(
        pool.clone(),
        &destination_token,
        "valid-reversal",
        original_id,
    )
    .await;
    let reversal_id: Uuid =
        serde_json::from_slice::<Value>(&to_bytes(reversal.into_body(), usize::MAX).await.unwrap())
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
    let nested = reversal_response(pool, &source_token, "nested-reversal", reversal_id).await;
    assert_eq!(nested.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        serde_json::from_slice::<Value>(&to_bytes(nested.into_body(), usize::MAX).await.unwrap())
            .unwrap()["error"]["code"],
        "reversal_of_reversal"
    );
}

#[sqlx::test]
async fn reversal_arithmetic_overflow_rolls_back_its_reservation(pool: PgPool) {
    let original_source = Uuid::new_v4();
    let original_destination = Uuid::new_v4();
    let original_id = Uuid::new_v4();
    insert_test_account(&pool, original_source, "client-123", "USD", 2, i64::MAX).await;
    insert_test_account(&pool, original_destination, "client-456", "USD", 2, 1).await;
    sqlx::query(
        "INSERT INTO transfers (id, source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, total_source_debit_minor, kind, initiated_by) VALUES ($1, $2, $3, 'USD', 'USD', 1, 1, 1, 'transfer', 'client-123')",
    )
    .bind(original_id)
    .bind(original_source)
    .bind(original_destination)
    .execute(&pool)
    .await
    .unwrap();
    for (account_id, counterparty_account_id, direction) in [
        (original_source, original_destination, "debit"),
        (original_destination, original_source, "credit"),
    ] {
        sqlx::query(
            "INSERT INTO account_entries (id, account_id, transfer_id, counterparty_account_id, direction, operation_kind, amount_minor, currency) VALUES ($1, $2, $3, $4, $5::entry_direction, 'transfer', 1, 'USD')",
        )
        .bind(Uuid::new_v4())
        .bind(account_id)
        .bind(original_id)
        .bind(counterparty_account_id)
        .bind(direction)
        .execute(&pool)
        .await
        .unwrap();
    }
    let token = token(
        Some("client-456"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let response = reversal_response(pool.clone(), &token, "overflow-reversal", original_id).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        serde_json::from_slice::<Value>(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
            .unwrap()["error"]["code"],
        "arithmetic_overflow"
    );
    for (id, balance) in [(original_source, i64::MAX), (original_destination, 1)] {
        assert_eq!(
            sqlx::query_as::<_, (i64, i64)>(
                "SELECT balance_minor, version FROM accounts WHERE id = $1"
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap(),
            (balance, 0)
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM transfers WHERE kind = 'reversal'")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM account_entries WHERE operation_kind = 'reversal'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM idempotency_records WHERE operation_type = 'reversal' AND idempotency_key = 'overflow-reversal'").fetch_one(&pool).await.unwrap(), 0);
}

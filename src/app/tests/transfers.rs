use super::support::*;

#[sqlx::test]
async fn transfer_is_atomic_auditable_and_replayable(pool: PgPool) {
    let source = Uuid::new_v4();
    let destination = Uuid::new_v4();
    insert_test_account(&pool, source, "client-123", "USD", 2, 2_000).await;
    insert_test_account(&pool, destination, "client-456", "USD", 2, 500).await;
    let owner_token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let request = json!({"source_account_id": source, "destination_account_id": destination, "amount": "10.2"});
    let first = transfer_response(pool.clone(), &owner_token, "transfer-key", request).await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first_body = to_bytes(first.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&first_body).unwrap();
    assert_eq!(body["amount"], "10.20");
    assert_eq!(body["resulting_source_balance"], "9.80");
    assert_eq!(body["resulting_destination_balance"], "15.20");
    assert_eq!(body["status"], "completed");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT balance_minor FROM accounts WHERE id = $1")
            .bind(source)
            .fetch_one(&pool)
            .await
            .unwrap(),
        980
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT balance_minor FROM accounts WHERE id = $1")
            .bind(destination)
            .fetch_one(&pool)
            .await
            .unwrap(),
        1_520
    );
    for id in [source, destination] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT version FROM accounts WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM transfers")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM account_entries")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM account_entries WHERE direction = 'debit'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM account_entries WHERE direction = 'credit'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        1
    );

    sqlx::query("UPDATE accounts SET owner_id = 'client-changed', status = 'active' WHERE id = $1")
        .bind(source)
        .execute(&pool)
        .await
        .unwrap();
    let replay = transfer_response(pool.clone(), &owner_token, "transfer-key", json!({"source_account_id": source, "destination_account_id": destination, "amount": "10.20"})).await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(
        to_bytes(replay.into_body(), usize::MAX).await.unwrap(),
        first_body
    );
    let conflict = transfer_response(pool.clone(), &owner_token, "transfer-key", json!({"source_account_id": source, "destination_account_id": destination, "amount": "10.21"})).await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
}

#[sqlx::test]
async fn transfer_failures_roll_back_the_reservation_and_balances(pool: PgPool) {
    let source = Uuid::new_v4();
    let destination = Uuid::new_v4();
    insert_test_account(&pool, source, "client-123", "USD", 2, 100).await;
    insert_test_account(&pool, destination, "client-456", "USD", 2, 0).await;
    let owner_token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let failed = transfer_response(
        pool.clone(),
        &owner_token,
        "retry-key",
        json!({"source_account_id":source,"destination_account_id":destination,"amount":"1.01"}),
    )
    .await;
    assert_eq!(failed.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM idempotency_records")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT balance_minor FROM accounts WHERE id = $1")
            .bind(source)
            .fetch_one(&pool)
            .await
            .unwrap(),
        100
    );
    let retry = transfer_response(
        pool.clone(),
        &owner_token,
        "retry-key",
        json!({"source_account_id":source,"destination_account_id":destination,"amount":"1.00"}),
    )
    .await;
    assert_eq!(retry.status(), StatusCode::CREATED);
}

#[sqlx::test]
async fn transfer_validates_input_and_hides_unavailable_sources(pool: PgPool) {
    let source = Uuid::new_v4();
    let destination = Uuid::new_v4();
    let foreign_source = Uuid::new_v4();
    let jpy_destination = Uuid::new_v4();
    insert_test_account(&pool, source, "client-123", "USD", 2, 100).await;
    insert_test_account(&pool, destination, "client-456", "USD", 2, 0).await;
    insert_test_account(&pool, foreign_source, "client-456", "USD", 2, 100).await;
    insert_test_account(&pool, jpy_destination, "client-456", "JPY", 0, 0).await;
    let token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    for (key, body, code) in [
        (
            "malformed-source",
            json!({"source_account_id":"not-a-uuid","destination_account_id":destination,"amount":"1"}),
            "malformed_account_id",
        ),
        (
            "same-account",
            json!({"source_account_id":source,"destination_account_id":source,"amount":"1"}),
            "same_source_and_destination",
        ),
        (
            "malformed-amount",
            json!({"source_account_id":source,"destination_account_id":destination,"amount":"one"}),
            "malformed_amount",
        ),
        (
            "zero",
            json!({"source_account_id":source,"destination_account_id":destination,"amount":"0"}),
            "non_positive_amount",
        ),
        (
            "negative",
            json!({"source_account_id":source,"destination_account_id":destination,"amount":"-1"}),
            "non_positive_amount",
        ),
        (
            "precision",
            json!({"source_account_id":source,"destination_account_id":destination,"amount":"0.001"}),
            "too_many_fractional_digits",
        ),
        (
            "overflow",
            json!({"source_account_id":source,"destination_account_id":destination,"amount":"92233720368547758.08"}),
            "amount_overflow",
        ),
    ] {
        let response = transfer_response(pool.clone(), &token, key, body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{key}");
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["error"]["code"], code);
    }
    let missing = transfer_response(pool.clone(), &token, "missing", json!({"source_account_id":Uuid::new_v4(),"destination_account_id":destination,"amount":"0.01"})).await;
    let foreign = transfer_response(pool.clone(), &token, "foreign", json!({"source_account_id":foreign_source,"destination_account_id":destination,"amount":"0.01"})).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        to_bytes(missing.into_body(), usize::MAX).await.unwrap(),
        to_bytes(foreign.into_body(), usize::MAX).await.unwrap()
    );
    let mismatch = transfer_response(pool.clone(), &token, "mismatch", json!({"source_account_id":source,"destination_account_id":jpy_destination,"amount":"0.01"})).await;
    assert_eq!(mismatch.status(), StatusCode::UNPROCESSABLE_ENTITY);
    for (account_id, key) in [
        (source, "inactive-source"),
        (destination, "inactive-destination"),
    ] {
        sqlx::query("UPDATE accounts SET status = 'inactive' WHERE id = $1")
            .bind(account_id)
            .execute(&pool)
            .await
            .unwrap();
        let response = transfer_response(pool.clone(), &token, key, json!({"source_account_id":source,"destination_account_id":destination,"amount":"0.01"})).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["error"]["code"], "account_unavailable");
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM idempotency_records WHERE idempotency_key = $1"
            )
            .bind(key)
            .fetch_one(&pool)
            .await
            .unwrap(),
            0
        );
        sqlx::query("UPDATE accounts SET status = 'active' WHERE id = $1")
            .bind(account_id)
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[sqlx::test]
async fn competing_debits_allow_only_affordable_transfers(pool: PgPool) {
    let source = Uuid::new_v4();
    let first_destination = Uuid::new_v4();
    let second_destination = Uuid::new_v4();
    for (id, owner, balance) in [
        (source, "client-123", 100),
        (first_destination, "client-456", 0),
        (second_destination, "client-789", 0),
    ] {
        insert_test_account(&pool, id, owner, "USD", 2, balance).await;
    }
    let token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let (first, second) = tokio::join!(
        transfer_response(
            pool.clone(),
            &token,
            "debit-one",
            json!({"source_account_id":source,"destination_account_id":first_destination,"amount":"0.60"})
        ),
        transfer_response(
            pool.clone(),
            &token,
            "debit-two",
            json!({"source_account_id":source,"destination_account_id":second_destination,"amount":"0.60"})
        ),
    );
    assert!(
        (first.status() == StatusCode::CREATED
            && second.status() == StatusCode::UNPROCESSABLE_ENTITY)
            || (first.status() == StatusCode::UNPROCESSABLE_ENTITY
                && second.status() == StatusCode::CREATED)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT balance_minor FROM accounts WHERE id = $1")
            .bind(source)
            .fetch_one(&pool)
            .await
            .unwrap(),
        40
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM transfers")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test]
async fn opposite_direction_transfers_finish_without_deadlock(pool: PgPool) {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    insert_test_account(&pool, first, "client-123", "USD", 2, 100).await;
    insert_test_account(&pool, second, "client-456", "USD", 2, 100).await;
    let first_token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let second_token = token(
        Some("client-456"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::join!(
            transfer_response(
                pool.clone(),
                &first_token,
                "forward",
                json!({"source_account_id":first,"destination_account_id":second,"amount":"0.25"})
            ),
            transfer_response(
                pool.clone(),
                &second_token,
                "reverse",
                json!({"source_account_id":second,"destination_account_id":first,"amount":"0.25"})
            ),
        )
    })
    .await
    .expect("opposite-direction transfers must not deadlock");
    assert_eq!(result.0.status(), StatusCode::CREATED);
    assert_eq!(result.1.status(), StatusCode::CREATED);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(sum(balance_minor), 0)::bigint FROM accounts"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        200
    );
}

#[sqlx::test]
async fn concurrent_same_key_requests_replay_or_conflict_once(pool: PgPool) {
    let source = Uuid::new_v4();
    let destination = Uuid::new_v4();
    insert_test_account(&pool, source, "client-123", "USD", 2, 1_000).await;
    insert_test_account(&pool, destination, "client-456", "USD", 2, 0).await;
    let token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let request =
        json!({"source_account_id":source,"destination_account_id":destination,"amount":"1.00"});
    let (first, second) = tokio::join!(
        transfer_response(pool.clone(), &token, "same-key", request.clone()),
        transfer_response(pool.clone(), &token, "same-key", request),
    );
    assert_eq!(first.status(), StatusCode::CREATED);
    assert_eq!(second.status(), StatusCode::CREATED);
    assert_eq!(
        to_bytes(first.into_body(), usize::MAX).await.unwrap(),
        to_bytes(second.into_body(), usize::MAX).await.unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM transfers")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );

    let (winner, conflict) = tokio::join!(
        transfer_response(
            pool.clone(),
            &token,
            "different-key",
            json!({"source_account_id":source,"destination_account_id":destination,"amount":"1.00"})
        ),
        transfer_response(
            pool.clone(),
            &token,
            "different-key",
            json!({"source_account_id":source,"destination_account_id":destination,"amount":"2.00"})
        ),
    );
    assert!(
        (winner.status() == StatusCode::CREATED && conflict.status() == StatusCode::CONFLICT)
            || (winner.status() == StatusCode::CONFLICT
                && conflict.status() == StatusCode::CREATED)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM transfers")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
}

#[sqlx::test]
async fn late_database_failure_rolls_back_every_transfer_write(pool: PgPool) {
    let source = Uuid::new_v4();
    let destination = Uuid::new_v4();
    insert_test_account(&pool, source, "client-123", "USD", 2, 500).await;
    insert_test_account(&pool, destination, "client-456", "USD", 2, 100).await;
    sqlx::query("CREATE FUNCTION reject_transfer_entry() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'forced transfer entry failure'; END; $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_transfer_entry BEFORE INSERT ON account_entries FOR EACH ROW EXECUTE FUNCTION reject_transfer_entry()").execute(&pool).await.unwrap();
    let token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let request =
        json!({"source_account_id":source,"destination_account_id":destination,"amount":"1.00"});
    let failed = transfer_response(pool.clone(), &token, "late-failure", request.clone()).await;
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    for id in [source, destination] {
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT version FROM accounts WHERE id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT balance_minor FROM accounts WHERE id = $1")
            .bind(source)
            .fetch_one(&pool)
            .await
            .unwrap(),
        500
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT balance_minor FROM accounts WHERE id = $1")
            .bind(destination)
            .fetch_one(&pool)
            .await
            .unwrap(),
        100
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM transfers")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM account_entries")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM idempotency_records")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query("DROP TRIGGER reject_transfer_entry ON account_entries")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP FUNCTION reject_transfer_entry()")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        transfer_response(pool, &token, "late-failure", request)
            .await
            .status(),
        StatusCode::CREATED
    );
}

use super::support::*;

#[tokio::test]
async fn account_validation_errors_have_stable_codes() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
        .unwrap();
    let token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    for (body, code) in [
        (
            json!({"currency":"GBP", "initial_balance":"1"}),
            "unsupported_currency",
        ),
        (
            json!({"currency":"PLN", "initial_balance":"x"}),
            "malformed_amount",
        ),
        (
            json!({"currency":"PLN", "initial_balance":"1.001"}),
            "too_many_fractional_digits",
        ),
        (
            json!({"currency":"PLN", "initial_balance":"-1"}),
            "negative_initial_balance",
        ),
        (
            json!({"currency":"PLN", "initial_balance":"92233720368547758.08"}),
            "amount_overflow",
        ),
    ] {
        let response =
            account_response(pool.clone(), "POST", "/accounts", &token, Some(body)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["error"]["code"], code);
    }
    let response =
        account_response(pool, "GET", "/accounts/not-a-uuid/balance", &token, None).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "malformed_account_id");
}

#[tokio::test]
async fn account_endpoints_require_the_existing_safe_authentication_envelope() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
        .unwrap();
    let request = Request::post("/accounts")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"currency":"PLN","initial_balance":"0"}"#))
        .unwrap();
    let response = router(pool, test_auth()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        r#"{"error":{"code":"unauthorized","message":"Authentication is required","details":null,"request_id":null}}"#
    );
}

#[tokio::test]
async fn json_extraction_failures_use_the_shared_safe_error_envelope() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
        .unwrap();
    let token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    for body in [
        r#"{"currency":"PLN","initial_balance":}"#,
        r#"{"currency":"PLN","initial_balance":1}"#,
    ] {
        let request = Request::post("/accounts")
            .header("authorization", format!("Bearer {token}"))
            .header("idempotency-key", "test-idempotency-key")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = router(pool.clone(), test_auth())
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("application/json")
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            std::str::from_utf8(&body).unwrap(),
            r#"{"error":{"code":"invalid_json","message":"The request body is invalid","details":null,"request_id":null}}"#
        );
    }
}

#[tokio::test]
async fn account_database_failures_use_the_shared_safe_error_envelope() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://user:password@127.0.0.1:1/ledger")
        .unwrap();
    let token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let response = account_response(
        pool,
        "POST",
        "/accounts",
        &token,
        Some(json!({"currency":"PLN", "initial_balance":"0"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        r#"{"error":{"code":"internal_error","message":"An internal error occurred","details":null,"request_id":null}}"#
    );
}

#[sqlx::test]
async fn accounts_are_created_owned_and_read_without_information_leaks(pool: PgPool) {
    let owner_token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let other_token = token(
        Some("client-456"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let response = account_response(
        pool.clone(),
        "POST",
        "/accounts",
        &owner_token,
        Some(json!({"currency":" pln ", "initial_balance":"0"})),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let created: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(created["currency"], "PLN");
    assert_eq!(created["balance"], "0.00");
    let id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
    let persisted: (Uuid, String, String, i16, i64, String, i64) = sqlx::query_as(
        "SELECT id, owner_id, currency, currency_scale, balance_minor, status::text, version FROM accounts WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        persisted,
        (
            id,
            "client-123".to_owned(),
            "PLN".to_owned(),
            2,
            0,
            "active".to_owned(),
            0,
        )
    );

    let response = account_response(
        pool.clone(),
        "GET",
        &format!("/accounts/{id}/balance"),
        &owner_token,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let balance: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(
        balance,
        json!({"id":id,"currency":"PLN","balance":"0.00","version":0})
    );

    let other = account_response(
        pool.clone(),
        "GET",
        &format!("/accounts/{id}/balance"),
        &other_token,
        None,
    )
    .await;
    let missing = account_response(
        pool,
        "GET",
        "/accounts/00000000-0000-0000-0000-000000000000/balance",
        &owner_token,
        None,
    )
    .await;
    assert_eq!(other.status(), StatusCode::NOT_FOUND);
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        to_bytes(other.into_body(), usize::MAX).await.unwrap(),
        to_bytes(missing.into_body(), usize::MAX).await.unwrap()
    );
}

#[sqlx::test]
async fn account_creation_replays_success_and_scopes_keys_by_client(pool: PgPool) {
    let owner_token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let other_token = token(
        Some("client-456"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let first = account_response_with_key(
        pool.clone(),
        "POST",
        "/accounts",
        &owner_token,
        "account-key",
        Some(json!({"currency":" pln ", "initial_balance":"10.2"})),
    )
    .await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let first_body = to_bytes(first.into_body(), usize::MAX).await.unwrap();

    let replay = account_response_with_key(
        pool.clone(),
        "POST",
        "/accounts",
        &owner_token,
        "account-key",
        Some(json!({"currency":"PLN", "initial_balance":"10.20"})),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_eq!(
        to_bytes(replay.into_body(), usize::MAX).await.unwrap(),
        first_body
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM accounts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT count(*) FROM idempotency_records WHERE http_status = 201 AND response_body IS NOT NULL").fetch_one(&pool).await.unwrap(), 1);

    let conflict = account_response_with_key(
        pool.clone(),
        "POST",
        "/accounts",
        &owner_token,
        "account-key",
        Some(json!({"currency":"USD", "initial_balance":"10.20"})),
    )
    .await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    let conflict_body: Value =
        serde_json::from_slice(&to_bytes(conflict.into_body(), usize::MAX).await.unwrap()).unwrap();
    assert_eq!(conflict_body["error"]["code"], "idempotency_conflict");

    let other_client = account_response_with_key(
        pool.clone(),
        "POST",
        "/accounts",
        &other_token,
        "account-key",
        Some(json!({"currency":"USD", "initial_balance":"10.20"})),
    )
    .await;
    assert_eq!(other_client.status(), StatusCode::CREATED);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM accounts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        2
    );
}

#[sqlx::test]
async fn failed_account_creation_does_not_retain_an_idempotency_reservation(pool: PgPool) {
    let owner_token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let failed = account_response_with_key(
        pool.clone(),
        "POST",
        "/accounts",
        &owner_token,
        "retry-key",
        Some(json!({"currency":"PLN", "initial_balance":"-1"})),
    )
    .await;
    assert_eq!(failed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM idempotency_records")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    let retry = account_response_with_key(
        pool.clone(),
        "POST",
        "/accounts",
        &owner_token,
        "retry-key",
        Some(json!({"currency":"PLN", "initial_balance":"1"})),
    )
    .await;
    assert_eq!(retry.status(), StatusCode::CREATED);
}

#[sqlx::test]
async fn failed_owning_account_creation_rolls_back_its_reservation(pool: PgPool) {
    sqlx::query(
        "CREATE FUNCTION reject_test_account_creation() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN RAISE EXCEPTION 'forced account creation failure'; END; $$",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_test_account_creation BEFORE INSERT ON accounts \
         FOR EACH ROW EXECUTE FUNCTION reject_test_account_creation()",
    )
    .execute(&pool)
    .await
    .unwrap();

    let owner_token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let failed = account_response_with_key(
        pool.clone(),
        "POST",
        "/accounts",
        &owner_token,
        "owning-retry-key",
        Some(json!({"currency":"PLN", "initial_balance":"1"})),
    )
    .await;
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM accounts")
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

    sqlx::query("DROP TRIGGER reject_test_account_creation ON accounts")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP FUNCTION reject_test_account_creation()")
        .execute(&pool)
        .await
        .unwrap();
    let retry = account_response_with_key(
        pool.clone(),
        "POST",
        "/accounts",
        &owner_token,
        "owning-retry-key",
        Some(json!({"currency":"PLN", "initial_balance":"1"})),
    )
    .await;
    assert_eq!(retry.status(), StatusCode::CREATED);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM accounts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

#[sqlx::test]
async fn concurrent_identical_account_creation_has_one_side_effect(pool: PgPool) {
    let owner_token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let request = json!({"currency":"PLN", "initial_balance":"1"});
    let (first, second) = tokio::join!(
        account_response_with_key(
            pool.clone(),
            "POST",
            "/accounts",
            &owner_token,
            "concurrent-key",
            Some(request.clone())
        ),
        account_response_with_key(
            pool.clone(),
            "POST",
            "/accounts",
            &owner_token,
            "concurrent-key",
            Some(request)
        ),
    );
    assert_eq!(first.status(), StatusCode::CREATED);
    assert_eq!(second.status(), StatusCode::CREATED);
    assert_eq!(
        to_bytes(first.into_body(), usize::MAX).await.unwrap(),
        to_bytes(second.into_body(), usize::MAX).await.unwrap()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM accounts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM idempotency_records WHERE http_status IS NULL"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
}

#[sqlx::test]
async fn concurrent_different_account_creation_fingerprints_conflict(pool: PgPool) {
    let owner_token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let (first, second) = tokio::join!(
        account_response_with_key(
            pool.clone(),
            "POST",
            "/accounts",
            &owner_token,
            "conflict-key",
            Some(json!({"currency":"PLN", "initial_balance":"1"})),
        ),
        account_response_with_key(
            pool.clone(),
            "POST",
            "/accounts",
            &owner_token,
            "conflict-key",
            Some(json!({"currency":"USD", "initial_balance":"1"})),
        ),
    );
    assert!(
        (first.status() == StatusCode::CREATED && second.status() == StatusCode::CONFLICT)
            || (first.status() == StatusCode::CONFLICT && second.status() == StatusCode::CREATED)
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM accounts")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
}

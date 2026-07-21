use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::IntoResponse,
};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::Serialize;
use serde_json::{Map, Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tower::ServiceExt;
use uuid::Uuid;

use crate::{api_error::AppError, auth::AuthVerifier, config::AuthConfig};

use super::router;

const TEST_PUBLIC_KEY: &str = include_str!("../../tests/fixtures/jwt-test-public.pem");
const TEST_PRIVATE_KEY: &str = include_str!("../../tests/fixtures/jwt-test-private.pem");
const OTHER_TEST_PRIVATE_KEY: &str =
    include_str!("../../tests/fixtures/jwt-other-test-private.pem");

#[derive(Serialize)]
struct TestClaims {
    sub: Option<String>,
    exp: usize,
    iss: String,
    aud: String,
}

fn test_auth() -> AuthVerifier {
    let config = AuthConfig::new("https://issuer.example", "ledger", TEST_PUBLIC_KEY).unwrap();
    AuthVerifier::new(&config).unwrap()
}

fn token(sub: Option<&str>, exp: usize, issuer: &str, audience: &str) -> String {
    token_with_key(
        sub,
        exp,
        issuer,
        audience,
        EncodingKey::from_rsa_pem(TEST_PRIVATE_KEY.as_bytes()).unwrap(),
    )
}

fn token_with_key(
    sub: Option<&str>,
    exp: usize,
    issuer: &str,
    audience: &str,
    key: EncodingKey,
) -> String {
    encode(
        &Header::new(Algorithm::RS256),
        &TestClaims {
            sub: sub.map(str::to_owned),
            exp,
            iss: issuer.to_owned(),
            aud: audience.to_owned(),
        },
        &key,
    )
    .unwrap()
}

async fn protected_response(authorization: Option<&str>) -> axum::response::Response {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
        .unwrap();
    let mut request = Request::get("/_test/authenticated")
        .body(Body::empty())
        .unwrap();
    if let Some(authorization) = authorization {
        request
            .headers_mut()
            .insert("authorization", authorization.parse().unwrap());
    }
    router(pool, test_auth()).oneshot(request).await.unwrap()
}

async fn account_response(
    pool: PgPool,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> axum::response::Response {
    account_response_with_key(pool, method, uri, token, "test-idempotency-key", body).await
}

async fn account_response_with_key(
    pool: PgPool,
    method: &str,
    uri: &str,
    token: &str,
    idempotency_key: &str,
    body: Option<Value>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("idempotency-key", idempotency_key);
    let body = match body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    router(pool, test_auth())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap()
}

async fn transfer_response(
    pool: PgPool,
    token: &str,
    idempotency_key: &str,
    body: Value,
) -> axum::response::Response {
    account_response_with_key(
        pool,
        "POST",
        "/transfers",
        token,
        idempotency_key,
        Some(body),
    )
    .await
}

async fn reversal_response(
    pool: PgPool,
    token: &str,
    idempotency_key: &str,
    transfer_id: Uuid,
) -> axum::response::Response {
    account_response_with_key(
        pool,
        "POST",
        &format!("/transfers/{transfer_id}/reversal"),
        token,
        idempotency_key,
        None,
    )
    .await
}

async fn insert_test_account(
    pool: &PgPool,
    id: Uuid,
    owner_id: &str,
    currency: &str,
    scale: i16,
    balance_minor: i64,
) {
    sqlx::query(
        "INSERT INTO accounts (id, owner_id, currency, currency_scale, balance_minor) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(owner_id)
    .bind(currency)
    .bind(scale)
    .bind(balance_minor)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn authenticated_client_extracts_valid_rs256_subject() {
    let token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let response = protected_response(Some(&format!("bEaReR {token}"))).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), br#"{"client_id":"client-123"}"#);
}

#[tokio::test]
async fn authentication_failures_are_safe_unauthorized_responses() {
    let valid = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let expired = token(Some("client-123"), 1, "https://issuer.example", "ledger");
    let wrong_issuer = token(
        Some("client-123"),
        4_102_444_800,
        "https://other.example",
        "ledger",
    );
    let wrong_audience = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "other",
    );
    let missing_subject = token(None, 4_102_444_800, "https://issuer.example", "ledger");
    let blank_subject = token(
        Some("  "),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let hs256 = encode(
        &Header::new(Algorithm::HS256),
        &TestClaims {
            sub: Some("client-123".into()),
            exp: 4_102_444_800,
            iss: "https://issuer.example".into(),
            aud: "ledger".into(),
        },
        &EncodingKey::from_secret(b"not-an-rsa-key"),
    )
    .unwrap();
    let cases = [
        None,
        Some("not-a-bearer-header".to_owned()),
        Some("Basic value".to_owned()),
        Some("Bearer ".to_owned()),
        Some(format!("Bearer {valid}x")),
        Some(format!("Bearer {expired}")),
        Some(format!("Bearer {wrong_issuer}")),
        Some(format!("Bearer {wrong_audience}")),
        Some(format!("Bearer {missing_subject}")),
        Some(format!("Bearer {blank_subject}")),
        Some(format!("Bearer {hs256}")),
    ];
    for authorization in cases {
        let response = protected_response(authorization.as_deref()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains("\"code\":\"unauthorized\""));
        assert!(!body.contains("client-123"));
        assert!(!body.contains("BEGIN"));
    }
}

#[tokio::test]
async fn duplicate_authorization_headers_are_rejected() {
    let token = token(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
    );
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
        .unwrap();
    let mut request = Request::get("/_test/authenticated")
        .body(Body::empty())
        .unwrap();
    request
        .headers_mut()
        .append("authorization", format!("Bearer {token}").parse().unwrap());
    request
        .headers_mut()
        .append("authorization", format!("Bearer {token}").parse().unwrap());

    let response = router(pool, test_auth()).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        r#"{"error":{"code":"unauthorized","message":"Authentication is required","details":null,"request_id":null}}"#
    );
}

#[tokio::test]
async fn token_signed_by_another_rsa_key_is_rejected() {
    let token = token_with_key(
        Some("client-123"),
        4_102_444_800,
        "https://issuer.example",
        "ledger",
        EncodingKey::from_rsa_pem(OTHER_TEST_PRIVATE_KEY.as_bytes()).unwrap(),
    );
    let response = protected_response(Some(&format!("Bearer {token}"))).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        r#"{"error":{"code":"unauthorized","message":"Authentication is required","details":null,"request_id":null}}"#
    );
}

fn assert_local_schema_references_resolve(value: &Value, schemas: &Map<String, Value>) {
    match value {
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str)
                && let Some(name) = reference.strip_prefix("#/components/schemas/")
            {
                assert!(
                    schemas.contains_key(name),
                    "unresolved schema reference: {reference}"
                );
            }
            for value in object.values() {
                assert_local_schema_references_resolve(value, schemas);
            }
        }
        Value::Array(values) => {
            for value in values {
                assert_local_schema_references_resolve(value, schemas);
            }
        }
        _ => {}
    }
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

#[tokio::test]
async fn shared_errors_have_stable_statuses_and_codes() {
    let cases = [
        (
            AppError::bad_request(Some(json!({"field": "amount"}))),
            StatusCode::BAD_REQUEST,
            "bad_request",
        ),
        (
            AppError::unauthorized(),
            StatusCode::UNAUTHORIZED,
            "unauthorized",
        ),
        (AppError::forbidden(), StatusCode::FORBIDDEN, "forbidden"),
        (AppError::not_found(), StatusCode::NOT_FOUND, "not_found"),
        (AppError::conflict(), StatusCode::CONFLICT, "conflict"),
        (
            AppError::service_unavailable(),
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
        ),
        (
            AppError::internal(anyhow::anyhow!("private database failure")),
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
        ),
    ];

    for (error, status, code) in cases {
        let response = error.into_response();
        assert_eq!(response.status(), status);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], code);
        assert_eq!(body["error"]["request_id"], Value::Null);
    }
}

#[tokio::test]
async fn internal_errors_do_not_expose_their_source() {
    let response = AppError::internal(anyhow::anyhow!("postgres://user:password@localhost/ledger"))
        .into_response();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = std::str::from_utf8(&body).unwrap();
    assert!(!body.contains("postgres://user:password"));
}

#[tokio::test]
async fn openapi_serves_documented_api_contract() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
        .unwrap();
    let response = router(pool, test_auth())
        .oneshot(Request::get("/openapi.json").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let document: Value = serde_json::from_slice(&body).unwrap();
    assert!(document["openapi"].as_str().unwrap().starts_with("3.1"));
    let paths = document["paths"].as_object().unwrap();
    assert_eq!(paths.len(), 6);
    assert!(paths.contains_key("/health"));
    assert!(paths.contains_key("/ready"));
    assert!(paths.contains_key("/accounts"));
    assert!(paths.contains_key("/accounts/{account_id}/balance"));
    assert!(paths.contains_key("/transfers"));
    assert!(paths.contains_key("/transfers/{transfer_id}/reversal"));
    assert!(paths["/ready"]["get"]["responses"].get("200").is_some());
    assert!(paths["/ready"]["get"]["responses"].get("503").is_some());
    assert!(paths["/health"]["get"].get("security").is_none());
    assert!(paths["/ready"]["get"].get("security").is_none());
    assert_eq!(
        paths["/accounts"]["post"]["security"][0]["bearerAuth"],
        json!([])
    );
    assert_eq!(
        paths["/accounts/{account_id}/balance"]["get"]["security"][0]["bearerAuth"],
        json!([])
    );
    assert_eq!(
        paths["/transfers"]["post"]["security"][0]["bearerAuth"],
        json!([])
    );
    assert_eq!(
        paths["/transfers/{transfer_id}/reversal"]["post"]["security"][0]["bearerAuth"],
        json!([])
    );
    let reversal = &paths["/transfers/{transfer_id}/reversal"]["post"];
    assert_eq!(reversal["parameters"][0]["name"], "transfer_id");
    assert_eq!(reversal["parameters"][0]["in"], "path");
    assert_eq!(reversal["parameters"][0]["required"], true);
    assert_eq!(reversal["parameters"][1]["name"], "Idempotency-Key");
    assert_eq!(reversal["parameters"][1]["in"], "header");
    assert_eq!(reversal["parameters"][1]["required"], true);
    for status in ["201", "400", "401", "404", "409", "422", "500"] {
        assert!(
            reversal["responses"].get(status).is_some(),
            "missing {status}"
        );
    }
    assert_eq!(
        reversal["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ReversalCreatedResponse"
    );
    let security_schemes = document["components"]["securitySchemes"]
        .as_object()
        .unwrap();
    assert_eq!(security_schemes["bearerAuth"]["type"], "http");
    assert_eq!(security_schemes["bearerAuth"]["scheme"], "bearer");
    assert_eq!(security_schemes["bearerAuth"]["bearerFormat"], "JWT");
    let schemas = document["components"]["schemas"].as_object().unwrap();
    assert!(schemas.contains_key("ErrorEnvelope"));
    assert!(schemas.contains_key("ApiErrorBody"));
    assert!(schemas.contains_key("CreateAccountRequest"));
    assert!(schemas.contains_key("ReversalCreatedResponse"));
    assert!(schemas.contains_key("AccountCreatedResponse"));
    assert!(schemas.contains_key("AccountBalanceResponse"));
    assert!(schemas.contains_key("CreateTransferRequest"));
    assert!(schemas.contains_key("TransferCreatedResponse"));
    assert_local_schema_references_resolve(&document, schemas);
    assert!(!std::str::from_utf8(&body).unwrap().contains("postgres://"));
}

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

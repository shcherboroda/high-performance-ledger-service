pub(super) use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::IntoResponse,
};
pub(super) use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
pub(super) use serde::Serialize;
pub(super) use serde_json::{Map, Value, json};
pub(super) use sqlx::{PgPool, postgres::PgPoolOptions};
pub(super) use tower::ServiceExt;
pub(super) use uuid::Uuid;

pub(super) use crate::{api_error::AppError, auth::AuthVerifier, config::AuthConfig};

pub(super) use super::super::router;

pub(super) const TEST_PUBLIC_KEY: &str =
    include_str!("../../../tests/fixtures/jwt-test-public.pem");
pub(super) const TEST_PRIVATE_KEY: &str =
    include_str!("../../../tests/fixtures/jwt-test-private.pem");
pub(super) const OTHER_TEST_PRIVATE_KEY: &str =
    include_str!("../../../tests/fixtures/jwt-other-test-private.pem");

#[derive(Serialize)]
pub(super) struct TestClaims {
    pub(super) sub: Option<String>,
    pub(super) exp: usize,
    pub(super) iss: String,
    pub(super) aud: String,
}

pub(super) fn test_auth() -> AuthVerifier {
    let config = AuthConfig::new("https://issuer.example", "ledger", TEST_PUBLIC_KEY).unwrap();
    AuthVerifier::new(&config).unwrap()
}

pub(super) fn token(sub: Option<&str>, exp: usize, issuer: &str, audience: &str) -> String {
    token_with_key(
        sub,
        exp,
        issuer,
        audience,
        EncodingKey::from_rsa_pem(TEST_PRIVATE_KEY.as_bytes()).unwrap(),
    )
}

pub(super) fn token_with_key(
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

pub(super) async fn protected_response(authorization: Option<&str>) -> axum::response::Response {
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

pub(super) async fn account_response(
    pool: PgPool,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> axum::response::Response {
    account_response_with_key(pool, method, uri, token, "test-idempotency-key", body).await
}

pub(super) async fn account_response_with_key(
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

pub(super) async fn transfer_response(
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

pub(super) async fn reversal_response(
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

pub(super) async fn insert_test_account(
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

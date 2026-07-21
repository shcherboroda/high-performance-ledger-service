use std::sync::LazyLock;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::error;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};

use crate::{
    api_error::{ApiErrorBody, AppError, ErrorEnvelope},
    auth::{AuthVerifier, AuthenticatedClient},
    money::{MoneyError, currency, format_minor_units, parse_initial_balance},
};

pub const SERVICE_TITLE: &str = "FJX High-Performance Ledger Service";
pub const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub auth: AuthVerifier,
}

pub fn router(pool: PgPool, auth: AuthVerifier) -> Router {
    let router = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/openapi.json", get(openapi))
        .route("/accounts", post(create_account))
        .route("/accounts/{account_id}/balance", get(get_balance));

    #[cfg(test)]
    let router = router.route("/_test/authenticated", get(test_authenticated));
    router.with_state(AppState { pool, auth })
}

#[cfg(test)]
async fn test_authenticated(
    crate::auth::AuthenticatedClient { client_id }: crate::auth::AuthenticatedClient,
) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "client_id": client_id }))
}

#[derive(Serialize, ToSchema)]
struct StatusResponse {
    status: &'static str,
}

#[utoipa::path(
    get,
    path = "/health",
    responses((status = 200, description = "Process is healthy", body = StatusResponse))
)]
async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(StatusResponse { status: "ok" }))
}

#[utoipa::path(
    get,
    path = "/ready",
    responses(
        (status = 200, description = "Database is reachable", body = StatusResponse),
        (status = 503, description = "Database is unavailable", body = ErrorEnvelope)
    )
)]
async fn ready(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(_) => Ok((StatusCode::OK, Json(StatusResponse { status: "ready" }))),
        Err(error) => {
            error!(error = %error, "PostgreSQL readiness check failed");
            Err(AppError::service_unavailable())
        }
    }
}

#[derive(Deserialize, ToSchema)]
struct CreateAccountRequest {
    #[schema(example = "PLN")]
    currency: String,
    #[schema(example = "10.25")]
    initial_balance: String,
}

#[derive(Serialize, ToSchema)]
struct AccountCreatedResponse {
    #[schema(example = "550e8400-e29b-41d4-a716-446655440000")]
    id: String,
    #[schema(example = "PLN")]
    currency: String,
    #[schema(value_type = String, example = "10.25")]
    balance: String,
}

#[derive(Serialize, ToSchema)]
struct AccountBalanceResponse {
    #[schema(example = "550e8400-e29b-41d4-a716-446655440000")]
    id: String,
    #[schema(example = "PLN")]
    currency: String,
    #[schema(value_type = String, example = "10.25")]
    balance: String,
    version: i64,
}

#[utoipa::path(
    post,
    path = "/accounts",
    request_body = CreateAccountRequest,
    security(("bearerAuth" = [])),
    responses(
        (status = 201, description = "Account created", body = AccountCreatedResponse),
        (status = 400, description = "Invalid currency or initial balance", body = ErrorEnvelope),
        (status = 401, description = "Authentication is required", body = ErrorEnvelope),
        (status = 500, description = "Internal failure", body = ErrorEnvelope)
    )
)]
async fn create_account(
    State(state): State<AppState>,
    AuthenticatedClient { client_id }: AuthenticatedClient,
    Json(request): Json<CreateAccountRequest>,
) -> Result<impl IntoResponse, AppError> {
    let currency = currency(&request.currency).ok_or_else(|| {
        AppError::validation("unsupported_currency", "The currency is not supported")
    })?;
    let balance_minor =
        parse_initial_balance(&request.initial_balance, currency.scale()).map_err(money_error)?;
    let (id,): (String,) = sqlx::query_as(
        "INSERT INTO accounts (id, owner_id, currency, currency_scale, balance_minor) \
         VALUES (md5(random()::text || clock_timestamp()::text)::uuid, $1, $2, $3, $4) \
         RETURNING id::text",
    )
    .bind(client_id)
    .bind(currency.code())
    .bind(i16::from(currency.scale()))
    .bind(balance_minor)
    .fetch_one(&state.pool)
    .await
    .map_err(AppError::internal)?;
    Ok((
        StatusCode::CREATED,
        Json(AccountCreatedResponse {
            id,
            currency: currency.code().to_owned(),
            balance: format_minor_units(balance_minor, currency.scale()),
        }),
    ))
}

#[utoipa::path(
    get,
    path = "/accounts/{account_id}/balance",
    params(("account_id" = String, Path, description = "Account UUID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "Current account balance", body = AccountBalanceResponse),
        (status = 400, description = "Invalid account ID", body = ErrorEnvelope),
        (status = 401, description = "Authentication is required", body = ErrorEnvelope),
        (status = 404, description = "Account not found", body = ErrorEnvelope),
        (status = 500, description = "Internal failure", body = ErrorEnvelope)
    )
)]
async fn get_balance(
    State(state): State<AppState>,
    AuthenticatedClient { client_id }: AuthenticatedClient,
    Path(account_id): Path<String>,
) -> Result<Json<AccountBalanceResponse>, AppError> {
    if !is_uuid(&account_id) {
        return Err(AppError::validation(
            "malformed_account_id",
            "The account ID is invalid",
        ));
    }
    let account = sqlx::query_as::<_, (String, i16, i64, i64)>(
        "SELECT currency, currency_scale, balance_minor, version \
         FROM accounts WHERE id = $1::uuid AND owner_id = $2",
    )
    .bind(&account_id)
    .bind(client_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(AppError::not_found)?;
    Ok(Json(AccountBalanceResponse {
        id: account_id,
        currency: account.0,
        balance: format_minor_units(account.2, account.1 as u8),
        version: account.3,
    }))
}

fn money_error(error: MoneyError) -> AppError {
    match error {
        MoneyError::Malformed => {
            AppError::validation("malformed_amount", "The amount is malformed")
        }
        MoneyError::TooManyFractionalDigits => AppError::validation(
            "too_many_fractional_digits",
            "The amount has too many fractional digits for this currency",
        ),
        MoneyError::Negative => AppError::validation(
            "negative_initial_balance",
            "The initial balance cannot be negative",
        ),
        MoneyError::Overflow => {
            AppError::validation("amount_overflow", "The amount is out of range")
        }
    }
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

#[derive(OpenApi)]
#[openapi(
    info(title = SERVICE_TITLE, version = SERVICE_VERSION),
    paths(health, ready, create_account, get_balance),
    components(schemas(StatusResponse, CreateAccountRequest, AccountCreatedResponse, AccountBalanceResponse, ErrorEnvelope, ApiErrorBody)),
    modifiers(&SecuritySchemeAddon)
)]
struct ApiDoc;

struct SecuritySchemeAddon;

impl Modify for SecuritySchemeAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        openapi
            .components
            .as_mut()
            .expect("OpenAPI components are generated from schemas")
            .add_security_scheme(
                "bearerAuth",
                SecurityScheme::Http(
                    HttpBuilder::new()
                        .scheme(HttpAuthScheme::Bearer)
                        .bearer_format("JWT")
                        .build(),
                ),
            );
    }
}

static OPENAPI: LazyLock<utoipa::openapi::OpenApi> = LazyLock::new(ApiDoc::openapi);

async fn openapi() -> Json<utoipa::openapi::OpenApi> {
    Json(OPENAPI.clone())
}

#[cfg(test)]
mod tests {
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

    use crate::{api_error::AppError, auth::AuthVerifier, config::AuthConfig};

    use super::router;

    const TEST_PUBLIC_KEY: &str = include_str!("../tests/fixtures/jwt-test-public.pem");
    const TEST_PRIVATE_KEY: &str = include_str!("../tests/fixtures/jwt-test-private.pem");
    const OTHER_TEST_PRIVATE_KEY: &str =
        include_str!("../tests/fixtures/jwt-other-test-private.pem");

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
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"));
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
        let response =
            AppError::internal(anyhow::anyhow!("postgres://user:password@localhost/ledger"))
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
        assert_eq!(paths.len(), 4);
        assert!(paths.contains_key("/health"));
        assert!(paths.contains_key("/ready"));
        assert!(paths.contains_key("/accounts"));
        assert!(paths.contains_key("/accounts/{account_id}/balance"));
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
        assert!(schemas.contains_key("AccountCreatedResponse"));
        assert!(schemas.contains_key("AccountBalanceResponse"));
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
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
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
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(created["currency"], "PLN");
        assert_eq!(created["balance"], "0.00");
        let id = created["id"].as_str().unwrap();
        let persisted: (String, String, String, i16, i64, String, i64) = sqlx::query_as(
            "SELECT id::text, owner_id, currency, currency_scale, balance_minor, status::text, version FROM accounts WHERE id = $1::uuid",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            persisted,
            (
                id.to_owned(),
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
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
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
}

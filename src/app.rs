use std::sync::LazyLock;

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use serde::Serialize;
use sqlx::PgPool;
use tracing::error;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};

use crate::{
    api_error::{ApiErrorBody, AppError, ErrorEnvelope},
    auth::AuthVerifier,
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
        .route("/openapi.json", get(openapi));

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

#[derive(OpenApi)]
#[openapi(
    info(title = SERVICE_TITLE, version = SERVICE_VERSION),
    paths(health, ready),
    components(schemas(StatusResponse, ErrorEnvelope, ApiErrorBody)),
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
    use sqlx::postgres::PgPoolOptions;
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
        assert_eq!(paths.len(), 2);
        assert!(paths.contains_key("/health"));
        assert!(paths.contains_key("/ready"));
        assert!(paths["/ready"]["get"]["responses"].get("200").is_some());
        assert!(paths["/ready"]["get"]["responses"].get("503").is_some());
        assert!(paths["/health"]["get"].get("security").is_none());
        assert!(paths["/ready"]["get"].get("security").is_none());
        let security_schemes = document["components"]["securitySchemes"]
            .as_object()
            .unwrap();
        assert_eq!(security_schemes["bearerAuth"]["type"], "http");
        assert_eq!(security_schemes["bearerAuth"]["scheme"], "bearer");
        assert_eq!(security_schemes["bearerAuth"]["bearerFormat"], "JWT");
        let schemas = document["components"]["schemas"].as_object().unwrap();
        assert!(schemas.contains_key("ErrorEnvelope"));
        assert!(schemas.contains_key("ApiErrorBody"));
        assert_local_schema_references_resolve(&document, schemas);
        assert!(!std::str::from_utf8(&body).unwrap().contains("postgres://"));
    }
}

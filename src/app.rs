use std::sync::LazyLock;

use axum::{Json, Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use serde::Serialize;
use sqlx::PgPool;
use tracing::error;
use utoipa::{OpenApi, ToSchema};

use crate::api_error::{AppError, ErrorEnvelope};

pub const SERVICE_TITLE: &str = "FJX High-Performance Ledger Service";
pub const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
}

pub fn router(pool: PgPool) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/openapi.json", get(openapi))
        .with_state(AppState { pool })
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
    components(schemas(StatusResponse, ErrorEnvelope))
)]
struct ApiDoc;

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
    use serde_json::{Value, json};
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;

    use crate::api_error::AppError;

    use super::router;

    #[tokio::test]
    async fn health_is_available_without_a_database_connection() {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
            .unwrap();
        let response = router(pool)
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
        let response = router(pool)
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
        let response = router(pool)
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
        assert!(
            document["components"]["schemas"]
                .get("ErrorEnvelope")
                .is_some()
        );
        assert!(!std::str::from_utf8(&body).unwrap().contains("postgres://"));
    }
}

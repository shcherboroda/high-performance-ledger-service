use axum::{Router, extract::State, http::StatusCode, response::IntoResponse, routing::get};
use serde::Serialize;
use sqlx::PgPool;
use tracing::error;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
}

pub fn router(pool: PgPool) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .with_state(AppState { pool })
}

#[derive(Serialize)]
struct StatusResponse {
    status: &'static str,
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, axum::Json(StatusResponse { status: "ok" }))
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(_) => (
            StatusCode::OK,
            axum::Json(StatusResponse { status: "ready" }),
        ),
        Err(error) => {
            error!(error = %error, "PostgreSQL readiness check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(StatusResponse {
                    status: "unavailable",
                }),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;

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
    }

    #[tokio::test]
    async fn readiness_hides_database_failures() {
        let pool = PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/ledger")
            .unwrap();
        let response = router(pool)
            .oneshot(Request::get("/ready").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}

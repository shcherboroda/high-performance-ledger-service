use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};

use serde::Serialize;

use utoipa::ToSchema;

use crate::{
    api_error::{AppError, ErrorEnvelope},
    app::AppState,
    observability::{ReadinessOutcome, ReadinessReason, record_readiness_check},
};

#[derive(Serialize, ToSchema)]
pub(crate) struct StatusResponse {
    status: &'static str,
}

#[utoipa::path(
    get,
    path = "/health",
    responses((status = 200, description = "Process is healthy", body = StatusResponse))
)]
pub(crate) async fn health() -> impl IntoResponse {
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
pub(crate) async fn ready(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let started = std::time::Instant::now();
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(_) => {
            record_readiness_check(
                ReadinessOutcome::Ready,
                ReadinessReason::None,
                started.elapsed().as_secs_f64(),
            );
            Ok((StatusCode::OK, Json(StatusResponse { status: "ready" })))
        }
        Err(_) => {
            record_readiness_check(
                ReadinessOutcome::NotReady,
                ReadinessReason::DatabaseUnavailable,
                started.elapsed().as_secs_f64(),
            );
            Err(AppError::service_unavailable())
        }
    }
}

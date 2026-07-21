use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};

use serde::Serialize;

use tracing::error;

use utoipa::ToSchema;

use crate::{
    api_error::{AppError, ErrorEnvelope},
    app::AppState,
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

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    api_error::{AppError, ErrorEnvelope},
    app::AppState,
    application::reversals::{self, ReverseTransferCommand},
    auth::AuthenticatedClient,
    idempotency::IdempotencyKey,
};

#[derive(Serialize, ToSchema)]
pub(crate) struct ReversalCreatedResponse {
    id: Uuid,
    original_transfer_id: Uuid,
    source_account_id: Uuid,
    destination_account_id: Uuid,
    source_currency: String,
    destination_currency: String,
    #[schema(value_type = String, example = "10.25")]
    source_amount: String,
    #[schema(value_type = String, example = "10.25")]
    destination_amount: String,
    /// Fee snapshot from the original operation; no fee is charged by the reversal itself.
    #[schema(value_type = String, example = "0.00")]
    original_fee_amount: String,
    #[schema(value_type = String, example = "10.25")]
    total_source_debit: String,
    kind: &'static str,
    status: &'static str,
    resulting_source_balance: String,
    resulting_destination_balance: String,
    created_at: String,
}

#[utoipa::path(
    post,
    path = "/transfers/{transfer_id}/reversal",
    params(
        ("transfer_id" = String, Path, description = "Original transfer UUID"),
        ("Idempotency-Key" = String, Header, description = "Visible ASCII key, 1 to 255 characters")
    ),
    security(("bearerAuth" = [])),
    responses(
        (status = 201, description = "Reversal created or stored successful replay", body = ReversalCreatedResponse),
        (status = 400, description = "Invalid transfer ID or Idempotency-Key", body = ErrorEnvelope),
        (status = 401, description = "Authentication is required", body = ErrorEnvelope),
        (status = 404, description = "Original transfer or a participating account is unavailable", body = ErrorEnvelope),
        (status = 409, description = "Idempotency key was previously used with a different request", body = ErrorEnvelope),
        (status = 422, description = "Transfer cannot be reversed", body = ErrorEnvelope),
        (status = 500, description = "Internal failure", body = ErrorEnvelope)
    )
)]
pub(crate) async fn reverse_transfer(
    State(state): State<AppState>,
    AuthenticatedClient { client_id }: AuthenticatedClient,
    headers: HeaderMap,
    Path(transfer_id): Path<String>,
) -> Result<axum::response::Response, AppError> {
    let idempotency_key = IdempotencyKey::from_headers(&headers)?;
    let result = reversals::reverse(
        &state.pool,
        state.idempotency_retention,
        ReverseTransferCommand {
            client_id: &client_id,
            idempotency_key: &idempotency_key,
            original_transfer_id: &transfer_id,
        },
    )
    .await?;
    let status = StatusCode::from_u16(result.http_status as u16).map_err(AppError::internal)?;
    Ok((status, Json(result.response_body)).into_response())
}

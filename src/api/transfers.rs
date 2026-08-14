use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    api_error::{AppError, ErrorEnvelope},
    app::AppState,
    application::transfers::{self, CreateTransferCommand},
    auth::AuthenticatedClient,
    idempotency::IdempotencyKey,
};

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreateTransferRequest {
    source_account_id: String,
    destination_account_id: String,
    #[schema(example = "10.25")]
    amount: String,
}

#[allow(dead_code)]
#[derive(Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum TransferCreatedResponse {
    Transfer {
        id: Uuid,
        status: &'static str,
        source_account_id: Uuid,
        destination_account_id: Uuid,
        #[schema(example = "EUR")]
        currency: String,
        #[schema(value_type = String, example = "100.00")]
        amount: String,
        created_at: String,
    },
    FxTransfer {
        id: Uuid,
        status: &'static str,
        source_account_id: Uuid,
        destination_account_id: Uuid,
        #[schema(example = "EUR")]
        source_currency: String,
        #[schema(value_type = String, example = "100.00")]
        source_amount: String,
        #[schema(example = "PLN")]
        destination_currency: String,
        #[schema(value_type = String, example = "432.15")]
        destination_amount: String,
        #[schema(value_type = String, example = "1.00")]
        fee_amount: String,
        #[schema(value_type = String, example = "101.00")]
        total_source_debit: String,
        created_at: String,
    },
}

#[derive(Serialize, ToSchema)]
pub(crate) struct TransferDetailsResponse {
    id: Uuid,
    source_account_id: Uuid,
    destination_account_id: Uuid,
    source_currency: String,
    destination_currency: String,
    #[schema(value_type = String, example = "10.25")]
    source_amount: String,
    #[schema(value_type = String, example = "10.25")]
    destination_amount: String,
    #[schema(value_type = String, example = "0.00")]
    fee_amount: String,
    #[schema(value_type = String, example = "10.25")]
    total_source_debit: String,
    fee_bps: Option<i32>,
    #[schema(value_type = Option<String>, example = "1.083500000000")]
    exchange_rate: Option<String>,
    exchange_rate_id: Option<Uuid>,
    kind: String,
    reverses_transfer_id: Option<Uuid>,
    created_at: String,
}

#[utoipa::path(
    get,
    path = "/transfers/{transfer_id}",
    params(("transfer_id" = String, Path, description = "Transfer UUID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "Immutable transfer snapshot", body = TransferDetailsResponse),
        (status = 400, description = "Invalid transfer ID", body = ErrorEnvelope),
        (status = 401, description = "Authentication is required", body = ErrorEnvelope),
        (status = 404, description = "Transfer not found", body = ErrorEnvelope),
        (status = 500, description = "Internal failure", body = ErrorEnvelope)
    )
)]
pub(crate) async fn get_transfer(
    State(state): State<AppState>,
    AuthenticatedClient { client_id }: AuthenticatedClient,
    Path(transfer_id): Path<String>,
) -> Result<Json<TransferDetailsResponse>, AppError> {
    let transfer_id = Uuid::parse_str(&transfer_id)
        .map_err(|_| AppError::validation("malformed_transfer_id", "The transfer ID is invalid"))?;
    let transfer = transfers::get_details(&state.pool, &client_id, transfer_id).await?;
    Ok(Json(TransferDetailsResponse {
        id: transfer.id,
        source_account_id: transfer.source_account_id,
        destination_account_id: transfer.destination_account_id,
        source_currency: transfer.source_currency,
        destination_currency: transfer.destination_currency,
        source_amount: transfer.source_amount,
        destination_amount: transfer.destination_amount,
        fee_amount: transfer.fee_amount,
        total_source_debit: transfer.total_source_debit,
        fee_bps: transfer.fee_bps,
        exchange_rate: transfer.exchange_rate,
        exchange_rate_id: transfer.exchange_rate_id,
        kind: transfer.kind,
        reverses_transfer_id: transfer.reverses_transfer_id,
        created_at: transfer.created_at,
    }))
}

#[utoipa::path(
    post,
    path = "/transfers",
    request_body = CreateTransferRequest,
    params(("Idempotency-Key" = String, Header, description = "Visible ASCII key, 1 to 255 characters")),
    security(("bearerAuth" = [])),
    responses(
        (status = 201, description = "Transfer created or stored successful replay", body = TransferCreatedResponse),
        (status = 400, description = "Invalid transfer request or Idempotency-Key", body = ErrorEnvelope),
        (status = 401, description = "Authentication is required", body = ErrorEnvelope),
        (status = 404, description = "A requested account is unavailable", body = ErrorEnvelope),
        (status = 409, description = "Idempotency key was previously used with a different request", body = ErrorEnvelope),
        (status = 422, description = "Transfer cannot be completed", body = ErrorEnvelope),
        (status = 500, description = "Internal failure", body = ErrorEnvelope)
    )
)]
pub(crate) async fn create_transfer(
    State(state): State<AppState>,
    AuthenticatedClient { client_id }: AuthenticatedClient,
    headers: HeaderMap,
    request: Result<Json<CreateTransferRequest>, JsonRejection>,
) -> Result<axum::response::Response, AppError> {
    let idempotency_key = IdempotencyKey::from_headers(&headers)?;
    let Json(request) =
        request.map_err(|_| AppError::validation("invalid_json", "The request body is invalid"))?;
    let result = transfers::create(
        &state.pool,
        state.idempotency_retention,
        CreateTransferCommand {
            client_id: &client_id,
            idempotency_key: &idempotency_key,
            source_account_id: &request.source_account_id,
            destination_account_id: &request.destination_account_id,
            amount: &request.amount,
        },
    )
    .await?;
    let status = StatusCode::from_u16(result.http_status as u16).map_err(AppError::internal)?;
    Ok((status, Json(result.response_body)).into_response())
}

#[cfg(test)]
mod response_tests {
    use super::*;

    #[test]
    fn create_response_variants_are_internally_tagged() {
        let id = Uuid::nil();
        let ordinary = serde_json::to_value(TransferCreatedResponse::Transfer {
            id,
            status: "completed",
            source_account_id: id,
            destination_account_id: id,
            currency: "EUR".into(),
            amount: "100.00".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        })
        .unwrap();
        assert_eq!(ordinary["kind"], "transfer");
        assert!(ordinary.get("transfer").is_none());
        assert!(ordinary.get("Transfer").is_none());
        assert!(ordinary.get("source_currency").is_none());

        let fx = serde_json::to_value(TransferCreatedResponse::FxTransfer {
            id,
            status: "completed",
            source_account_id: id,
            destination_account_id: id,
            source_currency: "EUR".into(),
            source_amount: "100.00".into(),
            destination_currency: "PLN".into(),
            destination_amount: "432.15".into(),
            fee_amount: "1.00".into(),
            total_source_debit: "101.00".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        })
        .unwrap();
        assert_eq!(fx["kind"], "fx_transfer");
        assert!(fx.get("fx_transfer").is_none());
        assert!(fx.get("FxTransfer").is_none());
        assert!(fx.get("currency").is_none());
    }
}

use sqlx::PgPool;
use uuid::Uuid;

use crate::{api_error::AppError, money::format_minor_units, persistence::transfers};

/// Transport-independent immutable transfer snapshot for an authorized participant.
pub(crate) struct TransferDetails {
    pub id: Uuid,
    pub source_account_id: Uuid,
    pub destination_account_id: Uuid,
    pub source_currency: String,
    pub destination_currency: String,
    pub source_amount: String,
    pub destination_amount: String,
    pub fee_amount: String,
    pub total_source_debit: String,
    pub fee_bps: Option<i32>,
    pub exchange_rate: Option<String>,
    pub exchange_rate_id: Option<Uuid>,
    pub kind: String,
    pub reverses_transfer_id: Option<Uuid>,
    pub created_at: String,
}

pub(crate) async fn get_details(
    pool: &PgPool,
    client_id: &str,
    transfer_id: Uuid,
) -> Result<TransferDetails, AppError> {
    let transfer = transfers::find_details(pool, transfer_id, client_id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(AppError::not_found)?;
    Ok(TransferDetails {
        id: transfer_id,
        source_account_id: transfer.source_account_id,
        destination_account_id: transfer.destination_account_id,
        source_currency: transfer.source_currency,
        destination_currency: transfer.destination_currency,
        source_amount: format_minor_units(transfer.source_amount_minor, transfer.source_scale),
        destination_amount: format_minor_units(
            transfer.destination_amount_minor,
            transfer.destination_scale,
        ),
        fee_amount: format_minor_units(transfer.fee_amount_minor, transfer.source_scale),
        total_source_debit: format_minor_units(
            transfer.total_source_debit_minor,
            transfer.source_scale,
        ),
        fee_bps: transfer.fee_bps,
        exchange_rate: transfer.exchange_rate,
        exchange_rate_id: transfer.exchange_rate_id,
        kind: transfer.kind,
        reverses_transfer_id: transfer.reverses_transfer_id,
        created_at: transfer.created_at,
    })
}

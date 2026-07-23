use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use std::time::Instant;

use serde::Serialize;

use utoipa::ToSchema;

use uuid::Uuid;

use crate::{
    api_error::{AppError, ErrorEnvelope},
    app::AppState,
    auth::AuthenticatedClient,
    idempotency::{self, IdempotencyKey, Reservation},
    money::format_minor_units,
    observability::{
        FinancialOperation, FinancialReason, IdempotencyOutcome, TerminalOutcome,
        TransactionObservation, record_operation_without_transaction,
    },
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

struct LockedAccount {
    id: Uuid,
    currency: String,
    scale: i16,
    balance_minor: i64,
    status: String,
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
    let original_transfer_id = Uuid::parse_str(&transfer_id)
        .map_err(|_| AppError::validation("malformed_transfer_id", "The transfer ID is invalid"))?;
    let fingerprint = idempotency::reversal_fingerprint(original_transfer_id);
    let mut transaction = match state.pool.begin().await {
        Ok(transaction) => transaction,
        Err(error) => {
            let error = AppError::internal(error);
            let (outcome, reason) = error.financial_metric_outcome();
            record_operation_without_transaction(FinancialOperation::Reversal, outcome, reason);
            return Err(error);
        }
    };
    let mut observation =
        TransactionObservation::started(FinancialOperation::Reversal, Instant::now());
    let result = async {
        if let Some((stored_fingerprint, http_status, response_body)) =
            idempotency::completed_success(
                &mut transaction,
                &client_id,
                idempotency::REVERSAL_OPERATION,
                &idempotency_key,
            )
            .await
            .map_err(AppError::internal)?
        {
            if stored_fingerprint != fingerprint {
                observation.idempotency(IdempotencyOutcome::Conflict);
                return Err(AppError::idempotency_conflict());
            }
            observation.idempotency(IdempotencyOutcome::Replay);
            let status = StatusCode::from_u16(http_status as u16).map_err(AppError::internal)?;
            return Ok((status, Json(response_body)).into_response());
        }
        match idempotency::reserve(
            &mut transaction,
            &client_id,
            idempotency::REVERSAL_OPERATION,
            &idempotency_key,
            &fingerprint,
            state.idempotency_retention,
        )
        .await
        .map_err(AppError::internal)?
        {
            Reservation::Replay {
                http_status,
                response_body,
            } => {
                observation.idempotency(IdempotencyOutcome::Replay);
                let status =
                    StatusCode::from_u16(http_status as u16).map_err(AppError::internal)?;
                return Ok((status, Json(response_body)).into_response());
            }
            Reservation::Conflict => {
                observation.idempotency(IdempotencyOutcome::Conflict);
                return Err(AppError::idempotency_conflict());
            }
            Reservation::Owned => observation.idempotency(IdempotencyOutcome::Owner),
        }

        let original = sqlx::query_as::<_, (Uuid, Uuid, String, String, i64, i64, i64, i64, Option<i32>, String, Option<String>, Option<Uuid>, String)>(
        "SELECT source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, fee_amount_minor, total_source_debit_minor, fee_bps, kind::text, exchange_rate::text, exchange_rate_id, initiated_by FROM transfers WHERE id = $1 FOR UPDATE",
    ).bind(original_transfer_id).fetch_optional(&mut *transaction).await.map_err(AppError::internal)?
        .ok_or_else(AppError::not_found)?;
        if original.9 == "reversal" {
            let error = AppError::business("reversal_of_reversal", "A reversal cannot be reversed");
            return Err(error);
        }
        let authorized: Option<String> =
            sqlx::query_scalar("SELECT owner_id FROM accounts WHERE id = $1")
                .bind(original.1)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(AppError::internal)?;
        if authorized.as_deref() != Some(&client_id) {
            let error = AppError::not_found();
            return Err(error);
        }
        let already_reversed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM transfers WHERE reverses_transfer_id = $1)",
        )
        .bind(original_transfer_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(AppError::internal)?;
        if already_reversed {
            let error = AppError::business(
                "transfer_already_reversed",
                "The transfer has already been reversed",
            );
            return Err(error);
        }

        let source_account_id = original.1;
        let destination_account_id = original.0;
        let accounts = sqlx::query_as::<_, (Uuid, String, String, i16, i64, String)>(
        "SELECT id, owner_id, currency, currency_scale, balance_minor, status::text FROM accounts WHERE id = ANY($1) ORDER BY id FOR UPDATE",
    ).bind(vec![source_account_id, destination_account_id]).fetch_all(&mut *transaction).await.map_err(AppError::internal)?;
        if accounts.len() != 2 {
            let error = AppError::account_unavailable();
            return Err(error);
        }
        let locked = accounts
            .into_iter()
            .map(
                |(id, _owner_id, currency, scale, balance_minor, status)| LockedAccount {
                    id,
                    currency,
                    scale,
                    balance_minor,
                    status,
                },
            )
            .collect::<Vec<_>>();
        let source = locked
            .iter()
            .find(|account| account.id == source_account_id)
            .expect("both locked accounts returned");
        let destination = locked
            .iter()
            .find(|account| account.id == destination_account_id)
            .expect("both locked accounts returned");
        if source.status != "active" || destination.status != "active" {
            let error = AppError::account_unavailable();
            return Err(error);
        }
        if source.currency != original.3 || destination.currency != original.2 {
            return Err(AppError::internal(anyhow::anyhow!(
                "original transfer currency metadata does not match accounts"
            )));
        }
        let source_balance = source
            .balance_minor
            .checked_sub(original.5)
            .ok_or_else(|| {
                AppError::business("arithmetic_overflow", "The reversal amount is out of range")
            })?;
        let destination_balance = destination
            .balance_minor
            .checked_add(original.7)
            .ok_or_else(|| {
                AppError::business("arithmetic_overflow", "The reversal amount is out of range")
            })?;
        let reversal_id = Uuid::new_v4();
        let created_at: String = sqlx::query_scalar(
        "INSERT INTO transfers (id, source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, fee_amount_minor, total_source_debit_minor, fee_bps, exchange_rate, exchange_rate_id, kind, reverses_transfer_id, initiated_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $6, $9, CAST($10 AS numeric), $11, 'reversal', $12, $13) RETURNING created_at::text",
    ).bind(reversal_id).bind(source_account_id).bind(destination_account_id).bind(&original.3).bind(&original.2).bind(original.5).bind(original.7).bind(original.6).bind(original.8).bind(original.10).bind(original.11).bind(original_transfer_id).bind(&client_id).fetch_one(&mut *transaction).await.map_err(AppError::internal)?;
        for (id, balance) in [
            (source_account_id, source_balance),
            (destination_account_id, destination_balance),
        ] {
            sqlx::query("UPDATE accounts SET balance_minor = $1, version = version + 1, updated_at = now() WHERE id = $2").bind(balance).bind(id).execute(&mut *transaction).await.map_err(AppError::internal)?;
        }
        for (account_id, counterparty_account_id, direction, amount, currency) in [
            (
                source_account_id,
                destination_account_id,
                "debit",
                original.5,
                &original.3,
            ),
            (
                destination_account_id,
                source_account_id,
                "credit",
                original.7,
                &original.2,
            ),
        ] {
            sqlx::query("INSERT INTO account_entries (id, account_id, transfer_id, counterparty_account_id, direction, operation_kind, amount_minor, currency) VALUES ($1, $2, $3, $4, $5::entry_direction, 'reversal', $6, $7)").bind(Uuid::new_v4()).bind(account_id).bind(reversal_id).bind(counterparty_account_id).bind(direction).bind(amount).bind(currency).execute(&mut *transaction).await.map_err(AppError::internal)?;
        }
        let response_body = serde_json::to_value(ReversalCreatedResponse {
            id: reversal_id,
            original_transfer_id,
            source_account_id,
            destination_account_id,
            source_currency: original.3.clone(),
            destination_currency: original.2.clone(),
            source_amount: format_minor_units(original.5, source.scale as u8),
            destination_amount: format_minor_units(original.7, destination.scale as u8),
            original_fee_amount: format_minor_units(original.6, destination.scale as u8),
            total_source_debit: format_minor_units(original.5, source.scale as u8),
            kind: "reversal",
            status: "completed",
            resulting_source_balance: format_minor_units(source_balance, source.scale as u8),
            resulting_destination_balance: format_minor_units(
                destination_balance,
                destination.scale as u8,
            ),
            created_at,
        })
        .map_err(AppError::internal)?;
        idempotency::store_success(
            &mut transaction,
            &client_id,
            idempotency::REVERSAL_OPERATION,
            &idempotency_key,
            StatusCode::CREATED.as_u16().into(),
            &response_body,
            reversal_id,
        )
        .await
        .map_err(AppError::internal)?;
        Ok((StatusCode::CREATED, Json(response_body)).into_response())
    }
    .await;

    match result {
        Ok(response) => match transaction.commit().await {
            Ok(()) => {
                observation.complete(TerminalOutcome::Success, FinancialReason::None);
                Ok(response)
            }
            Err(error) => {
                let error = AppError::internal(error);
                observation.reject(&error);
                Err(error)
            }
        },
        Err(error) => {
            let _ = transaction.rollback().await;
            observation.reject(&error);
            Err(error)
        }
    }
}

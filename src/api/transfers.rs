use axum::{
    Json,
    extract::{Path, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use std::time::Instant;

use sqlx::Acquire;

use serde::{Deserialize, Serialize};

use utoipa::ToSchema;

use uuid::Uuid;

use crate::{
    api_error::{AppError, ErrorEnvelope},
    app::AppState,
    application::transfers,
    auth::AuthenticatedClient,
    fx::{
        ArithmeticError, ConfigurationError, ExactRate, calculate_destination, calculate_fee,
        select_exchange_rate, select_fee_rule,
    },
    idempotency::{self, IdempotencyKey, Reservation},
    money::{MoneyError, format_minor_units, parse_minor_units},
    observability::{
        FinancialOperation, FinancialReason, IdempotencyOutcome, PoolAcquireOutcome,
        TerminalOutcome, TransactionObservation, record_transfer_pool_acquire,
    },
};

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreateTransferRequest {
    source_account_id: String,
    destination_account_id: String,
    #[schema(example = "10.25")]
    amount: String,
}

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

#[derive(Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TransferKind {
    Transfer,
    FxTransfer,
}

impl TransferKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Transfer => "transfer",
            Self::FxTransfer => "fx_transfer",
        }
    }
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

#[derive(Serialize, ToSchema)]
struct LockedAccount {
    id: Uuid,
    owner_id: String,
    currency: String,
    scale: i16,
    balance_minor: i64,
    status: String,
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
    let source_account_id = Uuid::parse_str(&request.source_account_id)
        .map_err(|_| AppError::validation("malformed_account_id", "The account ID is invalid"))?;
    let destination_account_id = Uuid::parse_str(&request.destination_account_id)
        .map_err(|_| AppError::validation("malformed_account_id", "The account ID is invalid"))?;
    if source_account_id == destination_account_id {
        return Err(AppError::validation(
            "same_source_and_destination",
            "The source and destination accounts must differ",
        ));
    }

    // Currency and scale are immutable account metadata. Read them before acquiring the
    // transaction so decimal validation and fingerprinting do not retain a pool connection.
    // The locked account read below compares this snapshot again before any balance mutation.
    let preflight_started = Instant::now();
    let mut preflight_connection = match state.pool.acquire().await {
        Ok(connection) => connection,
        Err(error) => {
            record_transfer_pool_acquire(
                PoolAcquireOutcome::Failure,
                preflight_started.elapsed().as_secs_f64(),
            );
            return Err(AppError::internal(error));
        }
    };
    let preliminary = sqlx::query_as::<_, (String, i16, String, i16)>(
        "SELECT source.currency, source.currency_scale, destination.currency, destination.currency_scale \
         FROM accounts source JOIN accounts destination ON destination.id = $2 WHERE source.id = $1",
    )
    .bind(source_account_id)
    .bind(destination_account_id)
    .fetch_optional(&mut *preflight_connection)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(AppError::account_unavailable)?;
    drop(preflight_connection);
    let operation_type = if preliminary.0 == preliminary.2 {
        if preliminary.1 != preliminary.3 {
            return Err(AppError::internal(anyhow::anyhow!(
                "same currency accounts have different scales"
            )));
        }
        idempotency::TRANSFER_OPERATION
    } else {
        idempotency::FX_TRANSFER_OPERATION
    };
    let source_scale = u8::try_from(preliminary.1).map_err(AppError::internal)?;
    let amount_minor =
        parse_minor_units(&request.amount, source_scale).map_err(transfer_money_error)?;
    if amount_minor <= 0 {
        return Err(AppError::validation(
            "non_positive_amount",
            "The amount must be greater than zero",
        ));
    }
    let fingerprint = idempotency::transfer_fingerprint(
        source_account_id,
        destination_account_id,
        &preliminary.0,
        amount_minor,
    );

    let acquire_started = Instant::now();
    let mut connection = match state.pool.acquire().await {
        Ok(connection) => {
            record_transfer_pool_acquire(
                PoolAcquireOutcome::Success,
                acquire_started.elapsed().as_secs_f64(),
            );
            connection
        }
        Err(error) => {
            record_transfer_pool_acquire(
                PoolAcquireOutcome::Failure,
                acquire_started.elapsed().as_secs_f64(),
            );
            return Err(AppError::internal(error));
        }
    };
    let mut transaction = connection.begin().await.map_err(AppError::internal)?;
    let transaction_started = Instant::now();
    let mut observation = TransactionObservation::started(
        if operation_type == idempotency::TRANSFER_OPERATION {
            FinancialOperation::Transfer
        } else {
            FinancialOperation::FxTransfer
        },
        transaction_started,
    );
    let result = async {
    if let Some((stored_fingerprint, http_status, response_body)) = idempotency::completed_success(
        &mut transaction,
        &client_id,
        operation_type,
        &idempotency_key,
    )
    .await
    .map_err(AppError::internal)?
    {
        if fingerprint != stored_fingerprint {
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
        operation_type,
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
            let status = StatusCode::from_u16(http_status as u16).map_err(AppError::internal)?;
            return Ok((status, Json(response_body)).into_response());
        }
        Reservation::Conflict => {
            observation.idempotency(IdempotencyOutcome::Conflict);
            return Err(AppError::idempotency_conflict());
        }
        Reservation::Owned => observation.idempotency(IdempotencyOutcome::Owner),
    }
    let operation_time = sqlx::query_scalar("SELECT transaction_timestamp()")
        .fetch_one(&mut *transaction)
        .await
        .map_err(AppError::internal)?;
    let fx_configuration = if operation_type == idempotency::FX_TRANSFER_OPERATION {
        let rate = select_exchange_rate(
            &mut transaction,
            &preliminary.0,
            &preliminary.2,
            operation_time,
        )
        .await
        .map_err(transfer_configuration_error)?;
        let fee = select_fee_rule(
            &mut transaction,
            &preliminary.0,
            &preliminary.2,
            operation_time,
        )
        .await
        .map_err(transfer_configuration_error)?;
        Some((rate, fee))
    } else {
        None
    };
    let accounts = sqlx::query_as::<_, (Uuid, String, String, i16, i64, String)>(
        "SELECT id, owner_id, currency, currency_scale, balance_minor, status::text \
         FROM accounts WHERE id = ANY($1) ORDER BY id FOR UPDATE",
    )
    .bind(vec![source_account_id, destination_account_id])
    .fetch_all(&mut *transaction)
    .await
    .map_err(AppError::internal)?;
    if accounts.len() != 2 {
        let error = AppError::account_unavailable();
        return Err(error);
    }
    let locked = accounts
        .into_iter()
        .map(
            |(id, owner_id, currency, scale, balance_minor, status)| LockedAccount {
                id,
                owner_id,
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
    if source.owner_id != client_id || source.status != "active" || destination.status != "active" {
        let error = AppError::account_unavailable();
        return Err(error);
    }
    if (
        source.currency.as_str(),
        source.scale,
        destination.currency.as_str(),
        destination.scale,
    ) != (
        preliminary.0.as_str(),
        preliminary.1,
        preliminary.2.as_str(),
        preliminary.3,
    ) {
        return Err(AppError::internal(anyhow::anyhow!(
            "account currency metadata changed during transfer"
        )));
    }
    let (
        kind,
        destination_amount_minor,
        fee_amount_minor,
        total_source_debit_minor,
        fee_bps,
        exchange_rate,
        exchange_rate_id,
    ) = match fx_configuration {
        None => (
            TransferKind::Transfer,
            amount_minor,
            0,
            amount_minor,
            None,
            None,
            None,
        ),
        Some((rate, fee)) => {
            let fee_calculation = calculate_fee(amount_minor, fee.fee_bps).map_err(transfer_arithmetic_error)?;
            let exact_rate = ExactRate::parse(&rate.rate).map_err(transfer_arithmetic_error)?;
            let destination_amount = calculate_destination(
                amount_minor,
                source.scale as u8,
                destination.scale as u8,
                &exact_rate,
            )
            .map_err(transfer_arithmetic_error)?;
            (
                TransferKind::FxTransfer,
                destination_amount,
                fee_calculation.fee_minor,
                fee_calculation.total_source_debit_minor,
                Some(fee.fee_bps),
                Some(rate.rate),
                Some(rate.id),
            )
        }
    };
    if source.balance_minor < total_source_debit_minor {
        let error = AppError::business(
            "insufficient_funds",
            "The source account has insufficient funds",
        );
        return Err(error);
    }
    let source_balance = source
        .balance_minor
        .checked_sub(total_source_debit_minor)
        .ok_or_else(|| {
            AppError::business("arithmetic_overflow", "The transfer amount is out of range")
        })?;
    let destination_balance = destination
        .balance_minor
        .checked_add(destination_amount_minor)
        .ok_or_else(|| {
            AppError::business("arithmetic_overflow", "The transfer amount is out of range")
        })?;
    let transfer_id = Uuid::new_v4();
    let created_at: String = sqlx::query_scalar(
        "INSERT INTO transfers (id, source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, fee_amount_minor, total_source_debit_minor, fee_bps, exchange_rate, exchange_rate_id, kind, initiated_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, CAST($11 AS numeric), $12, $13::transfer_kind, $14) RETURNING created_at::text",
    )
    .bind(transfer_id).bind(source_account_id).bind(destination_account_id).bind(&source.currency).bind(&destination.currency).bind(amount_minor).bind(destination_amount_minor).bind(fee_amount_minor).bind(total_source_debit_minor).bind(fee_bps).bind(exchange_rate).bind(exchange_rate_id).bind(kind.as_str()).bind(&client_id)
    .fetch_one(&mut *transaction).await.map_err(AppError::internal)?;
    for (id, balance) in [
        (source_account_id, source_balance),
        (destination_account_id, destination_balance),
    ] {
        sqlx::query("UPDATE accounts SET balance_minor = $1, version = version + 1, updated_at = now() WHERE id = $2")
            .bind(balance).bind(id).execute(&mut *transaction).await.map_err(AppError::internal)?;
    }
    let is_fx = matches!(kind, TransferKind::FxTransfer);
    let source_principal = is_fx.then_some(amount_minor);
    let source_fee = is_fx.then_some(fee_amount_minor);
    let destination_principal = is_fx.then_some(destination_amount_minor);
    for (account_id, counterparty_account_id, direction, amount, currency, principal, fee) in [
        (
            source_account_id,
            destination_account_id,
            "debit",
            total_source_debit_minor,
            &source.currency,
            source_principal,
            source_fee,
        ),
        (
            destination_account_id,
            source_account_id,
            "credit",
            destination_amount_minor,
            &destination.currency,
            destination_principal,
            None,
        ),
    ] {
        sqlx::query(
            "INSERT INTO account_entries (id, account_id, transfer_id, counterparty_account_id, direction, operation_kind, amount_minor, currency, principal_amount_minor, fee_amount_minor) \
             VALUES ($1, $2, $3, $4, $5::entry_direction, $6::transfer_kind, $7, $8, $9, $10)",
        )
        .bind(Uuid::new_v4()).bind(account_id).bind(transfer_id).bind(counterparty_account_id).bind(direction).bind(kind.as_str()).bind(amount).bind(currency).bind(principal).bind(fee)
        .execute(&mut *transaction).await.map_err(AppError::internal)?;
    }
    let response = match kind {
        TransferKind::Transfer => TransferCreatedResponse::Transfer {
            id: transfer_id,
            status: "completed",
            source_account_id,
            destination_account_id,
            currency: source.currency.clone(),
            amount: format_minor_units(amount_minor, source.scale as u8),
            created_at,
        },
        TransferKind::FxTransfer => TransferCreatedResponse::FxTransfer {
            id: transfer_id,
            status: "completed",
            source_account_id,
            destination_account_id,
            source_currency: source.currency.clone(),
            source_amount: format_minor_units(amount_minor, source.scale as u8),
            destination_currency: destination.currency.clone(),
            destination_amount: format_minor_units(
                destination_amount_minor,
                destination.scale as u8,
            ),
            fee_amount: format_minor_units(fee_amount_minor, source.scale as u8),
            total_source_debit: format_minor_units(total_source_debit_minor, source.scale as u8),
            created_at,
        },
    };
    let response_body = serde_json::to_value(response).map_err(AppError::internal)?;
    idempotency::store_success(
        &mut transaction,
        &client_id,
        operation_type,
        &idempotency_key,
        StatusCode::CREATED.as_u16().into(),
        &response_body,
        transfer_id,
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
fn transfer_money_error(error: MoneyError) -> AppError {
    match error {
        MoneyError::Malformed => {
            AppError::validation("malformed_amount", "The amount is malformed")
        }
        MoneyError::TooManyFractionalDigits => AppError::validation(
            "too_many_fractional_digits",
            "The amount has too many fractional digits for this currency",
        ),
        MoneyError::Negative => AppError::validation(
            "non_positive_amount",
            "The amount must be greater than zero",
        ),
        MoneyError::Overflow => {
            AppError::validation("amount_overflow", "The amount is out of range")
        }
    }
}

fn transfer_configuration_error(error: ConfigurationError) -> AppError {
    match error {
        ConfigurationError::RateUnavailable => {
            AppError::business("rate_unavailable", "An FX rate is unavailable")
        }
        ConfigurationError::RateAmbiguous => AppError::business(
            "rate_configuration_ambiguous",
            "FX rate configuration is ambiguous",
        ),
        ConfigurationError::FeeRuleUnavailable => {
            AppError::business("fee_rule_unavailable", "An FX fee rule is unavailable")
        }
        ConfigurationError::FeeRuleAmbiguous => AppError::business(
            "fee_rule_configuration_ambiguous",
            "FX fee rule configuration is ambiguous",
        ),
        ConfigurationError::Database(error) => AppError::internal(error),
    }
}

fn transfer_arithmetic_error(error: ArithmeticError) -> AppError {
    match error {
        ArithmeticError::DestinationTooSmall => AppError::business(
            "destination_amount_too_small",
            "The converted destination amount rounds to zero",
        ),
        ArithmeticError::Overflow => {
            AppError::business("arithmetic_overflow", "The transfer amount is out of range")
        }
        ArithmeticError::InvalidRate
        | ArithmeticError::InvalidScale
        | ArithmeticError::InvalidAmount => {
            AppError::internal(anyhow::anyhow!("invalid persisted FX configuration"))
        }
    }
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

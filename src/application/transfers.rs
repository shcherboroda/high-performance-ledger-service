use std::time::{Duration, Instant};

use sqlx::{Acquire, PgPool};
use uuid::Uuid;

use crate::{
    api_error::AppError,
    fx::{ArithmeticError, ConfigurationError, ExactRate, calculate_destination, calculate_fee},
    idempotency::{self, IdempotencyKey, Reservation},
    money::{MoneyError, format_minor_units, parse_minor_units},
    observability::{
        FinancialOperation, FinancialReason, IdempotencyOutcome, PoolAcquireOutcome,
        TerminalOutcome, TransactionObservation, record_transfer_pool_acquire,
    },
    persistence::{
        fx,
        transfers::{self, NewAccountEntry, NewTransfer},
    },
};

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

pub(crate) struct CreateTransferCommand<'a> {
    pub client_id: &'a str,
    pub idempotency_key: &'a IdempotencyKey,
    pub source_account_id: &'a str,
    pub destination_account_id: &'a str,
    pub amount: &'a str,
}

#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CreatedTransferResponse {
    Transfer {
        id: Uuid,
        status: &'static str,
        source_account_id: Uuid,
        destination_account_id: Uuid,
        currency: String,
        amount: String,
        created_at: String,
    },
    FxTransfer {
        id: Uuid,
        status: &'static str,
        source_account_id: Uuid,
        destination_account_id: Uuid,
        source_currency: String,
        source_amount: String,
        destination_currency: String,
        destination_amount: String,
        fee_amount: String,
        total_source_debit: String,
        created_at: String,
    },
}

#[derive(Clone, Copy)]
enum TransferKind {
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

pub(crate) struct CreateTransferResult {
    pub http_status: i32,
    pub response_body: serde_json::Value,
}

pub(crate) async fn create(
    pool: &PgPool,
    idempotency_retention: Duration,
    command: CreateTransferCommand<'_>,
) -> Result<CreateTransferResult, AppError> {
    let PreparedTransfer {
        source_account_id,
        destination_account_id,
        preliminary,
        amount_minor,
        operation_type,
        fingerprint,
    } = prepare_transfer(pool, &command).await?;
    let acquire_started = Instant::now();
    let mut connection = match pool.acquire().await {
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
    let mut observation = TransactionObservation::started(
        if operation_type == idempotency::TRANSFER_OPERATION {
            FinancialOperation::Transfer
        } else {
            FinancialOperation::FxTransfer
        },
        Instant::now(),
    );
    let result = async {
        if let Some((stored_fingerprint, http_status, response_body)) =
            idempotency::completed_success(
                &mut transaction,
                command.client_id,
                operation_type,
                command.idempotency_key,
            )
            .await
            .map_err(AppError::internal)?
        {
            if fingerprint != stored_fingerprint {
                observation.idempotency(IdempotencyOutcome::Conflict);
                return Err(AppError::idempotency_conflict());
            }
            observation.idempotency(IdempotencyOutcome::Replay);
            return Ok(CreateTransferResult {
                http_status,
                response_body,
            });
        }
        match idempotency::reserve(
            &mut transaction,
            command.client_id,
            operation_type,
            command.idempotency_key,
            &fingerprint,
            idempotency_retention,
        )
        .await
        .map_err(AppError::internal)?
        {
            Reservation::Replay {
                http_status,
                response_body,
            } => {
                observation.idempotency(IdempotencyOutcome::Replay);
                return Ok(CreateTransferResult {
                    http_status,
                    response_body,
                });
            }
            Reservation::Conflict => {
                observation.idempotency(IdempotencyOutcome::Conflict);
                return Err(AppError::idempotency_conflict());
            }
            Reservation::Owned => observation.idempotency(IdempotencyOutcome::Owner),
        }
        let operation_time = transfers::transaction_time(&mut transaction)
            .await
            .map_err(AppError::internal)?;
        let fx_configuration = if operation_type == idempotency::FX_TRANSFER_OPERATION {
            Some((
                fx::select_exchange_rate(
                    &mut transaction,
                    &preliminary.source_currency,
                    &preliminary.destination_currency,
                    operation_time,
                )
                .await
                .map_err(transfer_configuration_error)?,
                fx::select_fee_rule(
                    &mut transaction,
                    &preliminary.source_currency,
                    &preliminary.destination_currency,
                    operation_time,
                )
                .await
                .map_err(transfer_configuration_error)?,
            ))
        } else {
            None
        };
        let locked =
            transfers::lock_accounts(&mut transaction, source_account_id, destination_account_id)
                .await
                .map_err(AppError::internal)?;
        if locked.len() != 2 {
            return Err(AppError::account_unavailable());
        }
        let source = locked
            .iter()
            .find(|account| account.id == source_account_id)
            .expect("both locked accounts returned");
        let destination = locked
            .iter()
            .find(|account| account.id == destination_account_id)
            .expect("both locked accounts returned");
        if source.owner_id != command.client_id
            || source.status != "active"
            || destination.status != "active"
        {
            return Err(AppError::account_unavailable());
        }
        if (
            source.currency.as_str(),
            source.scale,
            destination.currency.as_str(),
            destination.scale,
        ) != (
            preliminary.source_currency.as_str(),
            preliminary.source_scale,
            preliminary.destination_currency.as_str(),
            preliminary.destination_scale,
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
                let fee_calculation =
                    calculate_fee(amount_minor, fee.fee_bps).map_err(transfer_arithmetic_error)?;
                let exact_rate = ExactRate::parse(&rate.rate).map_err(transfer_arithmetic_error)?;
                let destination_amount = calculate_destination(
                    amount_minor,
                    source.scale,
                    destination.scale,
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
            return Err(AppError::business(
                "insufficient_funds",
                "The source account has insufficient funds",
            ));
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
        let created_at = transfers::insert(
            &mut transaction,
            NewTransfer {
                id: transfer_id,
                source_account_id,
                destination_account_id,
                source_currency: &source.currency,
                destination_currency: &destination.currency,
                source_amount_minor: amount_minor,
                destination_amount_minor,
                fee_amount_minor,
                total_source_debit_minor,
                fee_bps,
                exchange_rate: exchange_rate.as_deref(),
                exchange_rate_id,
                kind: kind.as_str(),
                initiated_by: command.client_id,
            },
        )
        .await
        .map_err(AppError::internal)?;
        transfers::update_balance(&mut transaction, source_account_id, source_balance)
            .await
            .map_err(AppError::internal)?;
        transfers::update_balance(
            &mut transaction,
            destination_account_id,
            destination_balance,
        )
        .await
        .map_err(AppError::internal)?;
        let is_fx = matches!(kind, TransferKind::FxTransfer);
        transfers::insert_account_entry(
            &mut transaction,
            NewAccountEntry {
                account_id: source_account_id,
                transfer_id,
                counterparty_account_id: destination_account_id,
                direction: "debit",
                operation_kind: kind.as_str(),
                amount_minor: total_source_debit_minor,
                currency: &source.currency,
                principal_amount_minor: is_fx.then_some(amount_minor),
                fee_amount_minor: is_fx.then_some(fee_amount_minor),
            },
        )
        .await
        .map_err(AppError::internal)?;
        transfers::insert_account_entry(
            &mut transaction,
            NewAccountEntry {
                account_id: destination_account_id,
                transfer_id,
                counterparty_account_id: source_account_id,
                direction: "credit",
                operation_kind: kind.as_str(),
                amount_minor: destination_amount_minor,
                currency: &destination.currency,
                principal_amount_minor: is_fx.then_some(destination_amount_minor),
                fee_amount_minor: None,
            },
        )
        .await
        .map_err(AppError::internal)?;
        let response = match kind {
            TransferKind::Transfer => CreatedTransferResponse::Transfer {
                id: transfer_id,
                status: "completed",
                source_account_id,
                destination_account_id,
                currency: source.currency.clone(),
                amount: format_minor_units(amount_minor, source.scale),
                created_at,
            },
            TransferKind::FxTransfer => CreatedTransferResponse::FxTransfer {
                id: transfer_id,
                status: "completed",
                source_account_id,
                destination_account_id,
                source_currency: source.currency.clone(),
                source_amount: format_minor_units(amount_minor, source.scale),
                destination_currency: destination.currency.clone(),
                destination_amount: format_minor_units(destination_amount_minor, destination.scale),
                fee_amount: format_minor_units(fee_amount_minor, source.scale),
                total_source_debit: format_minor_units(total_source_debit_minor, source.scale),
                created_at,
            },
        };
        let response_body = serde_json::to_value(response).map_err(AppError::internal)?;
        idempotency::store_success(
            &mut transaction,
            command.client_id,
            operation_type,
            command.idempotency_key,
            201,
            &response_body,
            transfer_id,
        )
        .await
        .map_err(AppError::internal)?;
        Ok(CreateTransferResult {
            http_status: 201,
            response_body,
        })
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

struct PreparedTransfer {
    source_account_id: Uuid,
    destination_account_id: Uuid,
    preliminary: transfers::TransferMetadata,
    amount_minor: i64,
    operation_type: &'static str,
    fingerprint: String,
}

/// Validates the request and releases the preflight connection before CPU work or mutation.
async fn prepare_transfer(
    pool: &PgPool,
    command: &CreateTransferCommand<'_>,
) -> Result<PreparedTransfer, AppError> {
    let source_account_id = Uuid::parse_str(command.source_account_id)
        .map_err(|_| AppError::validation("malformed_account_id", "The account ID is invalid"))?;
    let destination_account_id = Uuid::parse_str(command.destination_account_id)
        .map_err(|_| AppError::validation("malformed_account_id", "The account ID is invalid"))?;
    if source_account_id == destination_account_id {
        return Err(AppError::validation(
            "same_source_and_destination",
            "The source and destination accounts must differ",
        ));
    }
    let started = Instant::now();
    let mut connection = match pool.acquire().await {
        Ok(connection) => connection,
        Err(error) => {
            record_transfer_pool_acquire(
                PoolAcquireOutcome::Failure,
                started.elapsed().as_secs_f64(),
            );
            return Err(AppError::internal(error));
        }
    };
    let preliminary =
        transfers::find_metadata(&mut connection, source_account_id, destination_account_id)
            .await
            .map_err(AppError::internal)?
            .ok_or_else(AppError::account_unavailable)?;
    drop(connection);
    let operation_type = if preliminary.source_currency == preliminary.destination_currency {
        if preliminary.source_scale != preliminary.destination_scale {
            return Err(AppError::internal(anyhow::anyhow!(
                "same currency accounts have different scales"
            )));
        }
        idempotency::TRANSFER_OPERATION
    } else {
        idempotency::FX_TRANSFER_OPERATION
    };
    let amount_minor = parse_minor_units(command.amount, preliminary.source_scale)
        .map_err(transfer_money_error)?;
    if amount_minor <= 0 {
        return Err(AppError::validation(
            "non_positive_amount",
            "The amount must be greater than zero",
        ));
    }
    let fingerprint = idempotency::transfer_fingerprint(
        source_account_id,
        destination_account_id,
        &preliminary.source_currency,
        amount_minor,
    );
    Ok(PreparedTransfer {
        source_account_id,
        destination_account_id,
        preliminary,
        amount_minor,
        operation_type,
        fingerprint,
    })
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

use std::time::{Duration, Instant};

use sqlx::{Acquire, PgPool, Postgres, Transaction};
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
        let plan = build_transfer_plan(
            &mut transaction,
            &command,
            &PreparedTransfer {
                source_account_id,
                destination_account_id,
                preliminary,
                amount_minor,
                operation_type,
                fingerprint,
            },
        )
        .await?;
        let persisted = persist_transfer(&mut transaction, &plan, command.client_id).await?;
        let response_body = create_response_body(&plan, &persisted)?;
        idempotency::store_success(
            &mut transaction,
            command.client_id,
            operation_type,
            command.idempotency_key,
            201,
            &response_body,
            persisted.id,
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

struct PersistedTransfer {
    id: Uuid,
    created_at: String,
}

/// Writes the transfer, both account balances, and their corresponding immutable ledger entries.
async fn persist_transfer(
    transaction: &mut Transaction<'_, Postgres>,
    plan: &TransferPlan,
    client_id: &str,
) -> Result<PersistedTransfer, AppError> {
    let id = Uuid::new_v4();
    let created_at = transfers::insert(
        transaction,
        NewTransfer {
            id,
            source_account_id: plan.source_account_id,
            destination_account_id: plan.destination_account_id,
            source_currency: &plan.source_currency,
            destination_currency: &plan.destination_currency,
            source_amount_minor: plan.source_amount_minor,
            destination_amount_minor: plan.destination_amount_minor,
            fee_amount_minor: plan.fee_amount_minor,
            total_source_debit_minor: plan.total_source_debit_minor,
            fee_bps: plan.fee_bps,
            exchange_rate: plan.exchange_rate.as_deref(),
            exchange_rate_id: plan.exchange_rate_id,
            kind: plan.kind.as_str(),
            initiated_by: client_id,
        },
    )
    .await
    .map_err(AppError::internal)?;
    transfers::update_balance(transaction, plan.source_account_id, plan.source_balance)
        .await
        .map_err(AppError::internal)?;
    transfers::update_balance(
        transaction,
        plan.destination_account_id,
        plan.destination_balance,
    )
    .await
    .map_err(AppError::internal)?;
    let is_fx = matches!(plan.kind, TransferKind::FxTransfer);
    transfers::insert_account_entry(
        transaction,
        NewAccountEntry {
            account_id: plan.source_account_id,
            transfer_id: id,
            counterparty_account_id: plan.destination_account_id,
            direction: "debit",
            operation_kind: plan.kind.as_str(),
            amount_minor: plan.total_source_debit_minor,
            currency: &plan.source_currency,
            principal_amount_minor: is_fx.then_some(plan.source_amount_minor),
            fee_amount_minor: is_fx.then_some(plan.fee_amount_minor),
        },
    )
    .await
    .map_err(AppError::internal)?;
    transfers::insert_account_entry(
        transaction,
        NewAccountEntry {
            account_id: plan.destination_account_id,
            transfer_id: id,
            counterparty_account_id: plan.source_account_id,
            direction: "credit",
            operation_kind: plan.kind.as_str(),
            amount_minor: plan.destination_amount_minor,
            currency: &plan.destination_currency,
            principal_amount_minor: is_fx.then_some(plan.destination_amount_minor),
            fee_amount_minor: None,
        },
    )
    .await
    .map_err(AppError::internal)?;
    Ok(PersistedTransfer { id, created_at })
}

/// Keeps the original successful response as the exact value retained for idempotent replays.
fn create_response_body(
    plan: &TransferPlan,
    persisted: &PersistedTransfer,
) -> Result<serde_json::Value, AppError> {
    let response = match plan.kind {
        TransferKind::Transfer => CreatedTransferResponse::Transfer {
            id: persisted.id,
            status: "completed",
            source_account_id: plan.source_account_id,
            destination_account_id: plan.destination_account_id,
            currency: plan.source_currency.clone(),
            amount: format_minor_units(plan.source_amount_minor, plan.source_scale),
            created_at: persisted.created_at.clone(),
        },
        TransferKind::FxTransfer => CreatedTransferResponse::FxTransfer {
            id: persisted.id,
            status: "completed",
            source_account_id: plan.source_account_id,
            destination_account_id: plan.destination_account_id,
            source_currency: plan.source_currency.clone(),
            source_amount: format_minor_units(plan.source_amount_minor, plan.source_scale),
            destination_currency: plan.destination_currency.clone(),
            destination_amount: format_minor_units(
                plan.destination_amount_minor,
                plan.destination_scale,
            ),
            fee_amount: format_minor_units(plan.fee_amount_minor, plan.source_scale),
            total_source_debit: format_minor_units(
                plan.total_source_debit_minor,
                plan.source_scale,
            ),
            created_at: persisted.created_at.clone(),
        },
    };
    serde_json::to_value(response).map_err(AppError::internal)
}

/// A fully authorized and funded transfer assembled from rows protected by `FOR UPDATE`.
struct TransferPlan {
    source_account_id: Uuid,
    destination_account_id: Uuid,
    source_currency: String,
    destination_currency: String,
    source_scale: u8,
    destination_scale: u8,
    source_amount_minor: i64,
    destination_amount_minor: i64,
    fee_amount_minor: i64,
    total_source_debit_minor: i64,
    fee_bps: Option<i32>,
    exchange_rate: Option<String>,
    exchange_rate_id: Option<Uuid>,
    kind: TransferKind,
    source_balance: i64,
    destination_balance: i64,
}

/// Re-reads immutable metadata under account locks before calculating any debit or credit.
async fn build_transfer_plan(
    transaction: &mut Transaction<'_, Postgres>,
    command: &CreateTransferCommand<'_>,
    prepared: &PreparedTransfer,
) -> Result<TransferPlan, AppError> {
    let operation_time = transfers::transaction_time(transaction)
        .await
        .map_err(AppError::internal)?;
    let fx_configuration = if prepared.operation_type == idempotency::FX_TRANSFER_OPERATION {
        Some((
            fx::select_exchange_rate(
                transaction,
                &prepared.preliminary.source_currency,
                &prepared.preliminary.destination_currency,
                operation_time,
            )
            .await
            .map_err(transfer_configuration_error)?,
            fx::select_fee_rule(
                transaction,
                &prepared.preliminary.source_currency,
                &prepared.preliminary.destination_currency,
                operation_time,
            )
            .await
            .map_err(transfer_configuration_error)?,
        ))
    } else {
        None
    };
    let locked = transfers::lock_accounts(
        transaction,
        prepared.source_account_id,
        prepared.destination_account_id,
    )
    .await
    .map_err(AppError::internal)?;
    if locked.len() != 2 {
        return Err(AppError::account_unavailable());
    }
    let source = locked
        .iter()
        .find(|account| account.id == prepared.source_account_id)
        .expect("both locked accounts returned");
    let destination = locked
        .iter()
        .find(|account| account.id == prepared.destination_account_id)
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
        prepared.preliminary.source_currency.as_str(),
        prepared.preliminary.source_scale,
        prepared.preliminary.destination_currency.as_str(),
        prepared.preliminary.destination_scale,
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
            prepared.amount_minor,
            0,
            prepared.amount_minor,
            None,
            None,
            None,
        ),
        Some((rate, fee)) => {
            let fee_calculation = calculate_fee(prepared.amount_minor, fee.fee_bps)
                .map_err(transfer_arithmetic_error)?;
            let exact_rate = ExactRate::parse(&rate.rate).map_err(transfer_arithmetic_error)?;
            let destination_amount = calculate_destination(
                prepared.amount_minor,
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
    Ok(TransferPlan {
        source_account_id: prepared.source_account_id,
        destination_account_id: prepared.destination_account_id,
        source_currency: source.currency.clone(),
        destination_currency: destination.currency.clone(),
        source_scale: source.scale,
        destination_scale: destination.scale,
        source_amount_minor: prepared.amount_minor,
        destination_amount_minor,
        fee_amount_minor,
        total_source_debit_minor,
        fee_bps,
        exchange_rate,
        exchange_rate_id,
        kind,
        source_balance,
        destination_balance,
    })
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

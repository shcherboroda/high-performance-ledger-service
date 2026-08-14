use std::time::{Duration, Instant};

use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    api_error::AppError,
    idempotency::{self, IdempotencyKey, Reservation},
    money::format_minor_units,
    observability::{
        FinancialOperation, FinancialReason, IdempotencyOutcome, TerminalOutcome,
        TransactionObservation, record_operation_without_transaction,
    },
    persistence::{
        reversals,
        transfers::{self, NewAccountEntry},
    },
};

pub(crate) struct ReverseTransferCommand<'a> {
    pub client_id: &'a str,
    pub idempotency_key: &'a IdempotencyKey,
    pub original_transfer_id: &'a str,
}

#[derive(serde::Serialize)]
struct ReversalResponse {
    id: Uuid,
    original_transfer_id: Uuid,
    source_account_id: Uuid,
    destination_account_id: Uuid,
    source_currency: String,
    destination_currency: String,
    source_amount: String,
    destination_amount: String,
    original_fee_amount: String,
    total_source_debit: String,
    kind: &'static str,
    status: &'static str,
    resulting_source_balance: String,
    resulting_destination_balance: String,
    created_at: String,
}

pub(crate) struct ReverseTransferResult {
    pub http_status: i32,
    pub response_body: serde_json::Value,
}

pub(crate) async fn reverse(
    pool: &PgPool,
    idempotency_retention: Duration,
    command: ReverseTransferCommand<'_>,
) -> Result<ReverseTransferResult, AppError> {
    let original_transfer_id = Uuid::parse_str(command.original_transfer_id)
        .map_err(|_| AppError::validation("malformed_transfer_id", "The transfer ID is invalid"))?;
    let fingerprint = idempotency::reversal_fingerprint(original_transfer_id);
    let mut transaction = match pool.begin().await {
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
                command.client_id,
                idempotency::REVERSAL_OPERATION,
                command.idempotency_key,
            )
            .await
            .map_err(AppError::internal)?
        {
            if stored_fingerprint != fingerprint {
                observation.idempotency(IdempotencyOutcome::Conflict);
                return Err(AppError::idempotency_conflict());
            }
            observation.idempotency(IdempotencyOutcome::Replay);
            return Ok(ReverseTransferResult {
                http_status,
                response_body,
            });
        }
        match idempotency::reserve(
            &mut transaction,
            command.client_id,
            idempotency::REVERSAL_OPERATION,
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
                return Ok(ReverseTransferResult {
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
        let original = reversals::find_original_for_update(&mut transaction, original_transfer_id)
            .await
            .map_err(AppError::internal)?
            .ok_or_else(AppError::not_found)?;
        if original.kind == "reversal" {
            return Err(AppError::business(
                "reversal_of_reversal",
                "A reversal cannot be reversed",
            ));
        }
        if !reversals::destination_is_owned_by(
            &mut transaction,
            original.destination_account_id,
            command.client_id,
        )
        .await
        .map_err(AppError::internal)?
        {
            return Err(AppError::not_found());
        }
        if reversals::exists_for_original(&mut transaction, original_transfer_id)
            .await
            .map_err(AppError::internal)?
        {
            return Err(AppError::business(
                "transfer_already_reversed",
                "The transfer has already been reversed",
            ));
        }
        let source_account_id = original.destination_account_id;
        let destination_account_id = original.source_account_id;
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
        if source.status != "active" || destination.status != "active" {
            return Err(AppError::account_unavailable());
        }
        if source.currency != original.destination_currency
            || destination.currency != original.source_currency
        {
            return Err(AppError::internal(anyhow::anyhow!(
                "original transfer currency metadata does not match accounts"
            )));
        }
        let source_balance = source
            .balance_minor
            .checked_sub(original.destination_amount_minor)
            .ok_or_else(|| {
                AppError::business("arithmetic_overflow", "The reversal amount is out of range")
            })?;
        let destination_balance = destination
            .balance_minor
            .checked_add(original.total_source_debit_minor)
            .ok_or_else(|| {
                AppError::business("arithmetic_overflow", "The reversal amount is out of range")
            })?;
        let reversal_id = Uuid::new_v4();
        let created_at = reversals::insert(
            &mut transaction,
            reversals::NewReversal {
                id: reversal_id,
                original_transfer_id,
                source_account_id,
                destination_account_id,
                source_currency: &original.destination_currency,
                destination_currency: &original.source_currency,
                source_amount_minor: original.destination_amount_minor,
                destination_amount_minor: original.total_source_debit_minor,
                original_fee_amount_minor: original.fee_amount_minor,
                fee_bps: original.fee_bps,
                exchange_rate: original.exchange_rate.as_deref(),
                exchange_rate_id: original.exchange_rate_id,
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
        for (account_id, counterparty_account_id, direction, amount, currency) in [
            (
                source_account_id,
                destination_account_id,
                "debit",
                original.destination_amount_minor,
                &original.destination_currency,
            ),
            (
                destination_account_id,
                source_account_id,
                "credit",
                original.total_source_debit_minor,
                &original.source_currency,
            ),
        ] {
            transfers::insert_account_entry(
                &mut transaction,
                NewAccountEntry {
                    account_id,
                    transfer_id: reversal_id,
                    counterparty_account_id,
                    direction,
                    operation_kind: "reversal",
                    amount_minor: amount,
                    currency,
                    principal_amount_minor: None,
                    fee_amount_minor: None,
                },
            )
            .await
            .map_err(AppError::internal)?;
        }
        let response_body = serde_json::to_value(ReversalResponse {
            id: reversal_id,
            original_transfer_id,
            source_account_id,
            destination_account_id,
            source_currency: original.destination_currency.clone(),
            destination_currency: original.source_currency.clone(),
            source_amount: format_minor_units(original.destination_amount_minor, source.scale),
            destination_amount: format_minor_units(
                original.total_source_debit_minor,
                destination.scale,
            ),
            original_fee_amount: format_minor_units(original.fee_amount_minor, destination.scale),
            total_source_debit: format_minor_units(original.destination_amount_minor, source.scale),
            kind: "reversal",
            status: "completed",
            resulting_source_balance: format_minor_units(source_balance, source.scale),
            resulting_destination_balance: format_minor_units(
                destination_balance,
                destination.scale,
            ),
            created_at,
        })
        .map_err(AppError::internal)?;
        idempotency::store_success(
            &mut transaction,
            command.client_id,
            idempotency::REVERSAL_OPERATION,
            command.idempotency_key,
            201,
            &response_body,
            reversal_id,
        )
        .await
        .map_err(AppError::internal)?;
        Ok(ReverseTransferResult {
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

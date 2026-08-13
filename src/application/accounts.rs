use std::time::Duration;

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    api_error::AppError,
    idempotency::{self, IdempotencyKey, Reservation},
    money::{MoneyError, currency, format_minor_units, parse_initial_balance},
    repository::accounts::{self, NewAccount},
};

/// Transport-independent input for the account-creation use case.
pub(crate) struct CreateAccountCommand<'a> {
    pub client_id: &'a str,
    pub idempotency_key: &'a IdempotencyKey,
    pub currency: &'a str,
    pub initial_balance: &'a str,
}

/// The committed account representation retained for idempotent replays.
#[derive(Serialize)]
pub(crate) struct CreatedAccount {
    pub id: Uuid,
    pub currency: String,
    pub balance: String,
}

pub(crate) enum CreateAccountResult {
    Created {
        response_body: serde_json::Value,
    },
    Replay {
        http_status: i32,
        response_body: serde_json::Value,
    },
}

/// Creates one account and its successful idempotency result in the same transaction.
pub(crate) async fn create_account(
    pool: &PgPool,
    idempotency_retention: Duration,
    command: CreateAccountCommand<'_>,
) -> Result<CreateAccountResult, AppError> {
    let currency = currency(command.currency).ok_or_else(|| {
        AppError::validation("unsupported_currency", "The currency is not supported")
    })?;
    let balance_minor =
        parse_initial_balance(command.initial_balance, currency.scale()).map_err(money_error)?;
    let fingerprint = idempotency::account_creation_fingerprint(currency.code(), balance_minor);
    let mut transaction = pool.begin().await.map_err(AppError::internal)?;

    match idempotency::reserve(
        &mut transaction,
        command.client_id,
        idempotency::ACCOUNT_CREATION_OPERATION,
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
            transaction.commit().await.map_err(AppError::internal)?;
            return Ok(CreateAccountResult::Replay {
                http_status,
                response_body,
            });
        }
        Reservation::Conflict => return Err(AppError::idempotency_conflict()),
        Reservation::Owned => {}
    }

    let account = CreatedAccount {
        id: Uuid::new_v4(),
        currency: currency.code().to_owned(),
        balance: format_minor_units(balance_minor, currency.scale()),
    };
    accounts::insert(
        &mut transaction,
        NewAccount {
            id: account.id,
            owner_id: command.client_id,
            currency: currency.code(),
            scale: currency.scale(),
            balance_minor,
        },
    )
    .await
    .map_err(AppError::internal)?;
    let response_body = serde_json::to_value(&account).map_err(AppError::internal)?;
    idempotency::store_success(
        &mut transaction,
        command.client_id,
        idempotency::ACCOUNT_CREATION_OPERATION,
        command.idempotency_key,
        201,
        &response_body,
        account.id,
    )
    .await
    .map_err(AppError::internal)?;
    transaction.commit().await.map_err(AppError::internal)?;
    Ok(CreateAccountResult::Created { response_body })
}

pub(crate) async fn get_balance(
    pool: &PgPool,
    client_id: &str,
    account_id: Uuid,
) -> Result<AccountBalance, AppError> {
    accounts::find_balance(pool, account_id, client_id)
        .await
        .map_err(AppError::internal)?
        .map(|account| AccountBalance {
            id: account_id,
            currency: account.currency,
            balance: format_minor_units(account.balance_minor, account.scale),
            version: account.version,
        })
        .ok_or_else(AppError::not_found)
}

pub(crate) struct AccountBalance {
    pub id: Uuid,
    pub currency: String,
    pub balance: String,
    pub version: i64,
}

fn money_error(error: MoneyError) -> AppError {
    match error {
        MoneyError::Malformed => {
            AppError::validation("malformed_amount", "The amount is malformed")
        }
        MoneyError::TooManyFractionalDigits => AppError::validation(
            "too_many_fractional_digits",
            "The amount has too many fractional digits for this currency",
        ),
        MoneyError::Negative => AppError::validation(
            "negative_initial_balance",
            "The initial balance cannot be negative",
        ),
        MoneyError::Overflow => {
            AppError::validation("amount_overflow", "The amount is out of range")
        }
    }
}

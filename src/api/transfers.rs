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
    auth::AuthenticatedClient,
    idempotency::{self, IdempotencyKey, Reservation},
    money::{MoneyError, currency, format_minor_units, parse_minor_units},
};

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreateTransferRequest {
    source_account_id: String,
    destination_account_id: String,
    #[schema(example = "10.25")]
    amount: String,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct TransferCreatedResponse {
    id: Uuid,
    source_account_id: Uuid,
    destination_account_id: Uuid,
    #[schema(example = "USD")]
    currency: String,
    #[schema(value_type = String, example = "10.25")]
    amount: String,
    status: &'static str,
    resulting_source_balance: String,
    resulting_destination_balance: String,
    created_at: String,
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
    type TransferRow = (
        Uuid,
        Uuid,
        String,
        String,
        i64,
        i64,
        i64,
        i64,
        Option<i32>,
        Option<String>,
        Option<Uuid>,
        String,
        Option<Uuid>,
        String,
        i16,
        i16,
    );
    let row = sqlx::query_as::<_, TransferRow>(
        "SELECT t.source_account_id, t.destination_account_id, t.source_currency, t.destination_currency, \
         t.source_amount_minor, t.destination_amount_minor, t.fee_amount_minor, t.total_source_debit_minor, \
         t.fee_bps, t.exchange_rate::text, t.exchange_rate_id, t.kind::text, t.reverses_transfer_id, \
         t.created_at::text, source.currency_scale, destination.currency_scale \
         FROM transfers t \
         JOIN accounts source ON source.id = t.source_account_id \
         JOIN accounts destination ON destination.id = t.destination_account_id \
         WHERE t.id = $1 AND (source.owner_id = $2 OR destination.owner_id = $2)",
    )
    .bind(transfer_id)
    .bind(client_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(AppError::not_found)?;
    Ok(Json(TransferDetailsResponse {
        id: transfer_id,
        source_account_id: row.0,
        destination_account_id: row.1,
        source_currency: row.2,
        destination_currency: row.3,
        source_amount: format_minor_units(row.4, row.14 as u8),
        destination_amount: format_minor_units(row.5, row.15 as u8),
        fee_amount: format_minor_units(row.6, row.14 as u8),
        total_source_debit: format_minor_units(row.7, row.14 as u8),
        fee_bps: row.8,
        exchange_rate: row.9,
        exchange_rate_id: row.10,
        kind: row.11,
        reverses_transfer_id: row.12,
        created_at: row.13,
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

    let mut transaction = state.pool.begin().await.map_err(AppError::internal)?;
    if let Some((stored_fingerprint, http_status, response_body)) = idempotency::completed_success(
        &mut transaction,
        &client_id,
        idempotency::TRANSFER_OPERATION,
        &idempotency_key,
    )
    .await
    .map_err(AppError::internal)?
    {
        let currency_code = response_body
            .get("currency")
            .and_then(serde_json::Value::as_str)
            .and_then(currency)
            .ok_or_else(|| {
                AppError::internal(anyhow::anyhow!("stored transfer response is invalid"))
            })?;
        let amount_minor = parse_minor_units(&request.amount, currency_code.scale())
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
            currency_code.code(),
            amount_minor,
        );
        if fingerprint != stored_fingerprint {
            return Err(AppError::idempotency_conflict());
        }
        transaction.commit().await.map_err(AppError::internal)?;
        let status = StatusCode::from_u16(http_status as u16).map_err(AppError::internal)?;
        return Ok((status, Json(response_body)).into_response());
    }
    let (fingerprint_currency, source_scale): (String, i16) =
        sqlx::query_as("SELECT currency, currency_scale FROM accounts WHERE id = $1")
            .bind(source_account_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(AppError::internal)?
            .ok_or_else(AppError::account_unavailable)?;
    let scale = u8::try_from(source_scale).map_err(AppError::internal)?;
    let amount_minor = parse_minor_units(&request.amount, scale).map_err(transfer_money_error)?;
    if amount_minor <= 0 {
        return Err(AppError::validation(
            "non_positive_amount",
            "The amount must be greater than zero",
        ));
    }
    let fingerprint = idempotency::transfer_fingerprint(
        source_account_id,
        destination_account_id,
        &fingerprint_currency,
        amount_minor,
    );
    match idempotency::reserve(
        &mut transaction,
        &client_id,
        idempotency::TRANSFER_OPERATION,
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
            transaction.commit().await.map_err(AppError::internal)?;
            let status = StatusCode::from_u16(http_status as u16).map_err(AppError::internal)?;
            return Ok((status, Json(response_body)).into_response());
        }
        Reservation::Conflict => return Err(AppError::idempotency_conflict()),
        Reservation::Owned => {}
    }
    let accounts = sqlx::query_as::<_, (Uuid, String, String, i16, i64, String)>(
        "SELECT id, owner_id, currency, currency_scale, balance_minor, status::text \
         FROM accounts WHERE id = ANY($1) ORDER BY id FOR UPDATE",
    )
    .bind(vec![source_account_id, destination_account_id])
    .fetch_all(&mut *transaction)
    .await
    .map_err(AppError::internal)?;
    if accounts.len() != 2 {
        return Err(AppError::account_unavailable());
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
        return Err(AppError::account_unavailable());
    }
    if source.currency != destination.currency || source.scale != destination.scale {
        return Err(AppError::business(
            "currency_mismatch",
            "The account currencies do not match",
        ));
    }
    if source.currency != fingerprint_currency || source.scale != source_scale {
        return Err(AppError::internal(anyhow::anyhow!(
            "account currency metadata changed during transfer"
        )));
    }
    if source.balance_minor < amount_minor {
        return Err(AppError::business(
            "insufficient_funds",
            "The source account has insufficient funds",
        ));
    }
    let source_balance = source
        .balance_minor
        .checked_sub(amount_minor)
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("source balance overflow")))?;
    let destination_balance = destination
        .balance_minor
        .checked_add(amount_minor)
        .ok_or_else(|| AppError::internal(anyhow::anyhow!("destination balance overflow")))?;
    let transfer_id = Uuid::new_v4();
    // Fee and rate fields preserve the original operation snapshot for auditability;
    // they are not an additional fee charged by the reversal.
    let created_at: String = sqlx::query_scalar(
        "INSERT INTO transfers (id, source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, total_source_debit_minor, kind, initiated_by) \
         VALUES ($1, $2, $3, $4, $4, $5, $5, $5, 'transfer', $6) RETURNING created_at::text",
    )
    .bind(transfer_id).bind(source_account_id).bind(destination_account_id).bind(&source.currency).bind(amount_minor).bind(&client_id)
    .fetch_one(&mut *transaction).await.map_err(AppError::internal)?;
    for (id, balance) in [
        (source_account_id, source_balance),
        (destination_account_id, destination_balance),
    ] {
        sqlx::query("UPDATE accounts SET balance_minor = $1, version = version + 1, updated_at = now() WHERE id = $2")
            .bind(balance).bind(id).execute(&mut *transaction).await.map_err(AppError::internal)?;
    }
    for (account_id, counterparty_account_id, direction) in [
        (source_account_id, destination_account_id, "debit"),
        (destination_account_id, source_account_id, "credit"),
    ] {
        sqlx::query(
            "INSERT INTO account_entries (id, account_id, transfer_id, counterparty_account_id, direction, operation_kind, amount_minor, currency) \
             VALUES ($1, $2, $3, $4, $5::entry_direction, 'transfer', $6, $7)",
        )
        .bind(Uuid::new_v4()).bind(account_id).bind(transfer_id).bind(counterparty_account_id).bind(direction).bind(amount_minor).bind(&source.currency)
        .execute(&mut *transaction).await.map_err(AppError::internal)?;
    }
    let response_body = serde_json::to_value(TransferCreatedResponse {
        id: transfer_id,
        source_account_id,
        destination_account_id,
        currency: source.currency.clone(),
        amount: format_minor_units(amount_minor, scale),
        status: "completed",
        resulting_source_balance: format_minor_units(source_balance, scale),
        resulting_destination_balance: format_minor_units(destination_balance, scale),
        created_at,
    })
    .map_err(AppError::internal)?;
    idempotency::store_success(
        &mut transaction,
        &client_id,
        idempotency::TRANSFER_OPERATION,
        &idempotency_key,
        StatusCode::CREATED.as_u16().into(),
        &response_body,
        transfer_id,
    )
    .await
    .map_err(AppError::internal)?;
    transaction.commit().await.map_err(AppError::internal)?;
    Ok((StatusCode::CREATED, Json(response_body)).into_response())
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

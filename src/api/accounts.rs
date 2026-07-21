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
    money::{MoneyError, currency, format_minor_units, parse_initial_balance},
};

#[derive(Deserialize, ToSchema)]
pub(crate) struct CreateAccountRequest {
    #[schema(example = "PLN")]
    currency: String,
    #[schema(example = "10.25")]
    initial_balance: String,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct AccountCreatedResponse {
    id: Uuid,
    #[schema(example = "PLN")]
    currency: String,
    #[schema(value_type = String, example = "10.25")]
    balance: String,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct AccountBalanceResponse {
    id: Uuid,
    #[schema(example = "PLN")]
    currency: String,
    #[schema(value_type = String, example = "10.25")]
    balance: String,
    version: i64,
}

#[utoipa::path(
    post,
    path = "/accounts",
    request_body = CreateAccountRequest,
    params(("Idempotency-Key" = String, Header, description = "Visible ASCII key, 1 to 255 characters")),
    security(("bearerAuth" = [])),
    responses(
        (status = 201, description = "Account created or stored successful replay", body = AccountCreatedResponse),
        (status = 400, description = "Invalid currency, initial balance, or Idempotency-Key", body = ErrorEnvelope),
        (status = 401, description = "Authentication is required", body = ErrorEnvelope),
        (status = 409, description = "Idempotency key was previously used with a different request", body = ErrorEnvelope),
        (status = 500, description = "Internal failure", body = ErrorEnvelope)
    )
)]
pub(crate) async fn create_account(
    State(state): State<AppState>,
    AuthenticatedClient { client_id }: AuthenticatedClient,
    headers: HeaderMap,
    request: Result<Json<CreateAccountRequest>, JsonRejection>,
) -> Result<axum::response::Response, AppError> {
    let idempotency_key = IdempotencyKey::from_headers(&headers)?;
    let Json(request) =
        request.map_err(|_| AppError::validation("invalid_json", "The request body is invalid"))?;
    let currency = currency(&request.currency).ok_or_else(|| {
        AppError::validation("unsupported_currency", "The currency is not supported")
    })?;
    let balance_minor =
        parse_initial_balance(&request.initial_balance, currency.scale()).map_err(money_error)?;
    let fingerprint = idempotency::account_creation_fingerprint(currency.code(), balance_minor);
    let mut transaction = state.pool.begin().await.map_err(AppError::internal)?;
    match idempotency::reserve(
        &mut transaction,
        &client_id,
        idempotency::ACCOUNT_CREATION_OPERATION,
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
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO accounts (id, owner_id, currency, currency_scale, balance_minor) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(&client_id)
    .bind(currency.code())
    .bind(i16::from(currency.scale()))
    .bind(balance_minor)
    .execute(&mut *transaction)
    .await
    .map_err(AppError::internal)?;
    let response_body = serde_json::to_value(AccountCreatedResponse {
        id,
        currency: currency.code().to_owned(),
        balance: format_minor_units(balance_minor, currency.scale()),
    })
    .map_err(AppError::internal)?;
    idempotency::store_success(
        &mut transaction,
        &client_id,
        idempotency::ACCOUNT_CREATION_OPERATION,
        &idempotency_key,
        StatusCode::CREATED.as_u16().into(),
        &response_body,
        id,
    )
    .await
    .map_err(AppError::internal)?;
    transaction.commit().await.map_err(AppError::internal)?;
    Ok((StatusCode::CREATED, Json(response_body)).into_response())
}

#[utoipa::path(
    get,
    path = "/accounts/{account_id}/balance",
    params(("account_id" = String, Path, description = "Account UUID")),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "Current account balance", body = AccountBalanceResponse),
        (status = 400, description = "Invalid account ID", body = ErrorEnvelope),
        (status = 401, description = "Authentication is required", body = ErrorEnvelope),
        (status = 404, description = "Account not found", body = ErrorEnvelope),
        (status = 500, description = "Internal failure", body = ErrorEnvelope)
    )
)]
pub(crate) async fn get_balance(
    State(state): State<AppState>,
    AuthenticatedClient { client_id }: AuthenticatedClient,
    Path(account_id): Path<String>,
) -> Result<Json<AccountBalanceResponse>, AppError> {
    let id = Uuid::parse_str(&account_id)
        .map_err(|_| AppError::validation("malformed_account_id", "The account ID is invalid"))?;
    let account = sqlx::query_as::<_, (String, i16, i64, i64)>(
        "SELECT currency, currency_scale, balance_minor, version \
         FROM accounts WHERE id = $1 AND owner_id = $2",
    )
    .bind(id)
    .bind(client_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(AppError::internal)?
    .ok_or_else(AppError::not_found)?;
    Ok(Json(AccountBalanceResponse {
        id,
        currency: account.0,
        balance: format_minor_units(account.2, account.1 as u8),
        version: account.3,
    }))
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

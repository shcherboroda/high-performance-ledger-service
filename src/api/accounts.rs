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
    application::accounts::{self, CreateAccountCommand, CreateAccountResult},
    auth::AuthenticatedClient,
    idempotency::IdempotencyKey,
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
    match accounts::create_account(
        &state.pool,
        state.idempotency_retention,
        CreateAccountCommand {
            client_id: &client_id,
            idempotency_key: &idempotency_key,
            currency: &request.currency,
            initial_balance: &request.initial_balance,
        },
    )
    .await?
    {
        CreateAccountResult::Created { response_body } => {
            Ok((StatusCode::CREATED, Json(response_body)).into_response())
        }
        CreateAccountResult::Replay {
            http_status,
            response_body,
        } => {
            let status = StatusCode::from_u16(http_status as u16).map_err(AppError::internal)?;
            Ok((status, Json(response_body)).into_response())
        }
    }
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
    let account = accounts::get_balance(&state.pool, &client_id, id).await?;
    Ok(Json(AccountBalanceResponse {
        id: account.id,
        currency: account.currency,
        balance: account.balance,
        version: account.version,
    }))
}

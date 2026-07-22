use axum::{
    Json,
    extract::{Path, RawQuery, State},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    api_error::{AppError, ErrorEnvelope},
    app::AppState,
    auth::AuthenticatedClient,
    money::format_minor_units,
};

#[derive(Serialize, ToSchema)]
pub(crate) struct AccountEntryResponse {
    entry_id: Uuid,
    transfer_id: Uuid,
    account_id: Uuid,
    counterparty_account_id: Uuid,
    direction: String,
    operation_kind: String,
    #[schema(value_type = String, example = "10.25")]
    amount: String,
    currency: String,
    #[schema(value_type = Option<String>, example = "10.00")]
    principal_amount: Option<String>,
    #[schema(value_type = Option<String>, example = "0.25")]
    fee_amount: Option<String>,
    created_at: String,
}

#[derive(Serialize, ToSchema)]
pub(crate) struct AccountHistoryResponse {
    items: Vec<AccountEntryResponse>,
    next_cursor: Option<String>,
}

#[derive(IntoParams)]
#[into_params(parameter_in = Query)]
#[allow(dead_code)]
struct HistoryQueryDocs {
    #[param(value_type = Option<String>, format = Uuid)]
    counterparty_account_id: Option<String>,
    #[param(minimum = 1, maximum = 100, default = 50)]
    limit: Option<u8>,
    #[param(value_type = Option<String>)]
    cursor: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    v: u8,
    account_id: Uuid,
    counterparty_account_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    entry_id: Uuid,
}

struct Query {
    counterparty_account_id: Option<Uuid>,
    limit: i64,
    cursor: Option<Cursor>,
}

#[utoipa::path(
    get,
    path = "/accounts/{account_id}/entries",
    params(("account_id" = String, Path, description = "Account UUID"), HistoryQueryDocs),
    security(("bearerAuth" = [])),
    responses(
        (status = 200, description = "Account-relative immutable ledger entries", body = AccountHistoryResponse),
        (status = 400, description = "Invalid account ID or pagination query", body = ErrorEnvelope),
        (status = 401, description = "Authentication is required", body = ErrorEnvelope),
        (status = 404, description = "Account not found", body = ErrorEnvelope),
        (status = 500, description = "Internal failure", body = ErrorEnvelope)
    )
)]
pub(crate) async fn get_account_history(
    State(state): State<AppState>,
    AuthenticatedClient { client_id }: AuthenticatedClient,
    Path(account_id): Path<String>,
    RawQuery(raw_query): RawQuery,
) -> Result<Json<AccountHistoryResponse>, AppError> {
    let account_id = Uuid::parse_str(&account_id)
        .map_err(|_| AppError::validation("malformed_account_id", "The account ID is invalid"))?;
    let query = parse_query(raw_query.as_deref(), account_id)?;
    let scale: i16 =
        sqlx::query_scalar("SELECT currency_scale FROM accounts WHERE id = $1 AND owner_id = $2")
            .bind(account_id)
            .bind(client_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(AppError::internal)?
            .ok_or_else(AppError::not_found)?;
    type EntryRow = (
        Uuid,
        Uuid,
        Uuid,
        String,
        String,
        i64,
        String,
        Option<i64>,
        Option<i64>,
        DateTime<Utc>,
    );
    let rows = if let Some(counterparty) = query.counterparty_account_id {
        sqlx::query_as::<_, EntryRow>(
            "SELECT id, transfer_id, counterparty_account_id, direction::text, operation_kind::text, amount_minor, currency, principal_amount_minor, fee_amount_minor, created_at \
             FROM account_entries WHERE account_id = $1 AND counterparty_account_id = $2 \
             AND ($3 IS NULL OR (created_at, id) < ($3, $4)) \
             ORDER BY created_at DESC, id DESC LIMIT $5",
        )
        .bind(account_id).bind(counterparty)
        .bind(query.cursor.as_ref().map(|cursor| cursor.created_at))
        .bind(query.cursor.as_ref().map(|cursor| cursor.entry_id))
        .bind(query.limit + 1)
        .fetch_all(&state.pool).await.map_err(AppError::internal)?
    } else {
        sqlx::query_as::<_, EntryRow>(
            "SELECT id, transfer_id, counterparty_account_id, direction::text, operation_kind::text, amount_minor, currency, principal_amount_minor, fee_amount_minor, created_at \
             FROM account_entries WHERE account_id = $1 \
             AND ($2 IS NULL OR (created_at, id) < ($2, $3)) \
             ORDER BY created_at DESC, id DESC LIMIT $4",
        )
        .bind(account_id)
        .bind(query.cursor.as_ref().map(|cursor| cursor.created_at))
        .bind(query.cursor.as_ref().map(|cursor| cursor.entry_id))
        .bind(query.limit + 1)
        .fetch_all(&state.pool).await.map_err(AppError::internal)?
    };
    let has_next = rows.len() > query.limit as usize;
    let entries = rows
        .into_iter()
        .take(query.limit as usize)
        .collect::<Vec<_>>();
    let next_cursor = has_next.then(|| {
        let last = entries
            .last()
            .expect("a page with a successor is non-empty");
        encode_cursor(&Cursor {
            v: 1,
            account_id,
            counterparty_account_id: query.counterparty_account_id,
            created_at: last.9,
            entry_id: last.0,
        })
    });
    let scale = u8::try_from(scale).map_err(AppError::internal)?;
    Ok(Json(AccountHistoryResponse {
        items: entries
            .into_iter()
            .map(|row| AccountEntryResponse {
                entry_id: row.0,
                transfer_id: row.1,
                account_id,
                counterparty_account_id: row.2,
                direction: row.3,
                operation_kind: row.4,
                amount: format_minor_units(row.5, scale),
                currency: row.6,
                principal_amount: row.7.map(|amount| format_minor_units(amount, scale)),
                fee_amount: row.8.map(|amount| format_minor_units(amount, scale)),
                created_at: row.9.to_rfc3339(),
            })
            .collect(),
        next_cursor,
    }))
}

fn parse_query(raw: Option<&str>, account_id: Uuid) -> Result<Query, AppError> {
    let mut counterparty = None;
    let mut limit = None;
    let mut cursor = None;
    for part in raw.unwrap_or("").split('&').filter(|part| !part.is_empty()) {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        match key {
            "counterparty_account_id" => {
                if counterparty.replace(value).is_some() {
                    return Err(invalid_query());
                }
            }
            "limit" => {
                if limit.replace(value).is_some() {
                    return Err(invalid_query());
                }
            }
            "cursor" if cursor.replace(value).is_some() => return Err(invalid_cursor()),
            "cursor" => {}
            _ => {}
        }
    }
    let counterparty_account_id = counterparty
        .map(|value| {
            Uuid::parse_str(value).map_err(|_| {
                AppError::validation(
                    "malformed_counterparty_account_id",
                    "The counterparty account ID is invalid",
                )
            })
        })
        .transpose()?;
    let limit = match limit {
        None => 50,
        Some(value) => value
            .parse::<i64>()
            .ok()
            .filter(|value| (1..=100).contains(value))
            .ok_or_else(invalid_query)?,
    };
    let cursor = cursor.map(decode_cursor).transpose()?;
    if let Some(cursor) = &cursor
        && (cursor.account_id != account_id
            || cursor.counterparty_account_id != counterparty_account_id)
    {
        return Err(invalid_cursor());
    }
    Ok(Query {
        counterparty_account_id,
        limit,
        cursor,
    })
}

fn invalid_query() -> AppError {
    AppError::validation(
        "invalid_limit",
        "The limit must be an integer from 1 to 100",
    )
}
fn invalid_cursor() -> AppError {
    AppError::validation("invalid_cursor", "The cursor is invalid")
}

fn encode_cursor(cursor: &Cursor) -> String {
    let bytes = serde_json::to_vec(cursor).expect("cursor serialization cannot fail");
    let mut output = String::from("v1.");
    for byte in bytes {
        use std::fmt::Write;
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn decode_cursor(value: &str) -> Result<Cursor, AppError> {
    let hex = value.strip_prefix("v1.").ok_or_else(invalid_cursor)?;
    if hex.is_empty() || hex.len() % 2 != 0 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid_cursor());
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).map_err(|_| invalid_cursor()))
        .collect::<Result<Vec<_>, _>>()?;
    let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| invalid_cursor())?;
    if cursor.v != 1 {
        return Err(invalid_cursor());
    }
    Ok(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cursor_round_trips_and_binds_query_shape() {
        let account_id = Uuid::new_v4();
        let cursor = Cursor {
            v: 1,
            account_id,
            counterparty_account_id: None,
            created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            entry_id: Uuid::new_v4(),
        };
        let encoded = encode_cursor(&cursor);
        assert_eq!(decode_cursor(&encoded).unwrap().account_id, account_id);
        assert!(parse_query(Some(&format!("cursor={encoded}")), Uuid::new_v4()).is_err());
        assert!(decode_cursor("v2.00").is_err());
    }
}

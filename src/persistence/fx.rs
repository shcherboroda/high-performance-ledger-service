use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::fx::{ConfigurationError, ExchangeRate, FeeRule};

/// Selects one applicable direct rate. Ordering is diagnostic-only: two rows fail closed.
pub async fn select_exchange_rate(
    connection: &mut PgConnection,
    source_currency: &str,
    destination_currency: &str,
    operation_time: DateTime<Utc>,
) -> Result<ExchangeRate, ConfigurationError> {
    let rows = sqlx::query_as::<_, (Uuid, String, String, String)>(
        "SELECT id, source_currency, destination_currency, rate::text AS rate
         FROM exchange_rates
         WHERE source_currency = $1 AND destination_currency = $2
           AND valid_from <= $3 AND valid_until > $3
         ORDER BY valid_from, id LIMIT 2",
    )
    .bind(source_currency)
    .bind(destination_currency)
    .bind(operation_time)
    .fetch_all(&mut *connection)
    .await
    .map_err(ConfigurationError::Database)?;
    match rows.as_slice() {
        [] => Err(ConfigurationError::RateUnavailable),
        [(id, source_currency, destination_currency, rate)] => Ok(ExchangeRate {
            id: *id,
            source_currency: source_currency.clone(),
            destination_currency: destination_currency.clone(),
            rate: rate.clone(),
        }),
        _ => Err(ConfigurationError::RateAmbiguous),
    }
}

/// Pair-specific rules take precedence; ambiguity is evaluated within one specificity level.
pub async fn select_fee_rule(
    connection: &mut PgConnection,
    source_currency: &str,
    destination_currency: &str,
    operation_time: DateTime<Utc>,
) -> Result<FeeRule, ConfigurationError> {
    let pair = pair_fee_rows(
        connection,
        source_currency,
        destination_currency,
        operation_time,
    )
    .await?;
    match pair.as_slice() {
        [rule] => return Ok(rule.clone()),
        [_, ..] => return Err(ConfigurationError::FeeRuleAmbiguous),
        [] => {}
    }
    let defaults = default_fee_rows(connection, operation_time).await?;
    match defaults.as_slice() {
        [] => Err(ConfigurationError::FeeRuleUnavailable),
        [rule] => Ok(rule.clone()),
        _ => Err(ConfigurationError::FeeRuleAmbiguous),
    }
}

async fn pair_fee_rows(
    connection: &mut PgConnection,
    source_currency: &str,
    destination_currency: &str,
    operation_time: DateTime<Utc>,
) -> Result<Vec<FeeRule>, ConfigurationError> {
    let rows = sqlx::query_as::<_, (Uuid, i32)>(
        "SELECT id, fee_bps FROM fx_fee_rules
         WHERE source_currency = $1 AND destination_currency = $2
           AND valid_from <= $3 AND valid_until > $3
         ORDER BY valid_from, id LIMIT 2",
    )
    .bind(source_currency)
    .bind(destination_currency)
    .bind(operation_time)
    .fetch_all(&mut *connection)
    .await
    .map_err(ConfigurationError::Database)?;
    Ok(rows
        .into_iter()
        .map(|(id, fee_bps)| FeeRule { id, fee_bps })
        .collect())
}

async fn default_fee_rows(
    connection: &mut PgConnection,
    operation_time: DateTime<Utc>,
) -> Result<Vec<FeeRule>, ConfigurationError> {
    let rows = sqlx::query_as::<_, (Uuid, i32)>(
        "SELECT id, fee_bps FROM fx_fee_rules
         WHERE source_currency IS NULL AND destination_currency IS NULL
           AND valid_from <= $1 AND valid_until > $1
         ORDER BY valid_from, id LIMIT 2",
    )
    .bind(operation_time)
    .fetch_all(&mut *connection)
    .await
    .map_err(ConfigurationError::Database)?;
    Ok(rows
        .into_iter()
        .map(|(id, fee_bps)| FeeRule { id, fee_bps })
        .collect())
}

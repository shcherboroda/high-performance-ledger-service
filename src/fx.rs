//! FX configuration selection and exact arithmetic for a future transfer workflow.
//!
//! Stored rates can have a coefficient near `10^30`; combining one with an `i64`
//! source amount and scale factors can yield a numerator near `10^66`. That exceeds
//! `i128`, so conversion deliberately uses `BigInt` until the checked `i64` result.

use chrono::{DateTime, Utc};
use num_bigint::BigInt;
use sqlx::{FromRow, PgConnection};
use std::{error::Error, fmt};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct ExchangeRate {
    pub id: Uuid,
    pub source_currency: String,
    pub destination_currency: String,
    pub rate: String,
}

#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct FeeRule {
    pub id: Uuid,
    pub fee_bps: i32,
}

#[derive(Debug)]
pub enum ConfigurationError {
    RateUnavailable,
    RateAmbiguous,
    FeeRuleUnavailable,
    FeeRuleAmbiguous,
    Database(sqlx::Error),
}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::RateUnavailable => "FX rate is unavailable",
            Self::RateAmbiguous => "FX rate configuration is ambiguous",
            Self::FeeRuleUnavailable => "FX fee rule is unavailable",
            Self::FeeRuleAmbiguous => "FX fee rule configuration is ambiguous",
            Self::Database(_) => "FX configuration query failed",
        })
    }
}

impl Error for ConfigurationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

/// Selects one applicable direct rate. Ordering is diagnostic-only: two rows fail closed.
pub async fn select_exchange_rate(
    connection: &mut PgConnection,
    source_currency: &str,
    destination_currency: &str,
    operation_time: DateTime<Utc>,
) -> Result<ExchangeRate, ConfigurationError> {
    let rows = sqlx::query_as::<_, ExchangeRate>(
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
        [rate] => Ok(rate.clone()),
        _ => Err(ConfigurationError::RateAmbiguous),
    }
}

/// Pair-specific rules take precedence; ambiguity is only evaluated within a specificity level.
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
    sqlx::query_as(
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
    .map_err(ConfigurationError::Database)
}

async fn default_fee_rows(
    connection: &mut PgConnection,
    operation_time: DateTime<Utc>,
) -> Result<Vec<FeeRule>, ConfigurationError> {
    sqlx::query_as(
        "SELECT id, fee_bps FROM fx_fee_rules
         WHERE source_currency IS NULL AND destination_currency IS NULL
           AND valid_from <= $1 AND valid_until > $1
         ORDER BY valid_from, id LIMIT 2",
    )
    .bind(operation_time)
    .fetch_all(&mut *connection)
    .await
    .map_err(ConfigurationError::Database)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactRate {
    coefficient: BigInt,
    scale: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithmeticError {
    InvalidRate,
    InvalidScale,
    InvalidAmount,
    Overflow,
    DestinationTooSmall,
}

impl ExactRate {
    pub fn parse(input: &str) -> Result<Self, ArithmeticError> {
        let (whole, fractional) = input.split_once('.').unwrap_or((input, ""));
        if whole.is_empty()
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || !fractional.bytes().all(|b| b.is_ascii_digit())
            || whole.len() > 18
            || fractional.len() > 12
            || whole.len() + fractional.len() > 30
            || input.matches('.').count() > 1
        {
            return Err(ArithmeticError::InvalidRate);
        }
        let coefficient = format!("{whole}{fractional}")
            .parse::<BigInt>()
            .map_err(|_| ArithmeticError::InvalidRate)?;
        if coefficient <= BigInt::from(0) {
            return Err(ArithmeticError::InvalidRate);
        }
        Ok(Self {
            coefficient,
            scale: fractional.len() as u8,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeeCalculation {
    pub fee_minor: i64,
    pub total_source_debit_minor: i64,
}

pub fn calculate_fee(source_minor: i64, fee_bps: i32) -> Result<FeeCalculation, ArithmeticError> {
    if source_minor <= 0 || !(0..=10_000).contains(&fee_bps) {
        return Err(ArithmeticError::InvalidAmount);
    }
    let fee = half_up_divide(
        BigInt::from(source_minor) * BigInt::from(fee_bps),
        BigInt::from(10_000),
    )?;
    let fee_minor = i64::try_from(fee).map_err(|_| ArithmeticError::Overflow)?;
    let total_source_debit_minor = source_minor
        .checked_add(fee_minor)
        .ok_or(ArithmeticError::Overflow)?;
    Ok(FeeCalculation {
        fee_minor,
        total_source_debit_minor,
    })
}

pub fn calculate_destination(
    source_minor: i64,
    source_scale: u8,
    destination_scale: u8,
    rate: &ExactRate,
) -> Result<i64, ArithmeticError> {
    if source_minor <= 0 {
        return Err(ArithmeticError::InvalidAmount);
    }
    if source_scale > 18 || destination_scale > 18 {
        return Err(ArithmeticError::InvalidScale);
    }
    let numerator = BigInt::from(source_minor) * &rate.coefficient * pow10(destination_scale);
    let denominator = pow10(rate.scale) * pow10(source_scale);
    let rounded = half_up_divide(numerator, denominator)?;
    if rounded == BigInt::from(0) {
        return Err(ArithmeticError::DestinationTooSmall);
    }
    i64::try_from(rounded).map_err(|_| ArithmeticError::Overflow)
}

fn pow10(scale: u8) -> BigInt {
    BigInt::from(10_u8).pow(u32::from(scale))
}

fn half_up_divide(numerator: BigInt, denominator: BigInt) -> Result<BigInt, ArithmeticError> {
    if denominator <= BigInt::from(0) {
        return Err(ArithmeticError::InvalidAmount);
    }
    let quotient = &numerator / &denominator;
    let remainder = numerator % &denominator;
    Ok(if remainder * 2 >= denominator {
        quotient + 1
    } else {
        quotient
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rate_parsing_and_conversion_are_exact() {
        assert_eq!(ExactRate::parse("1.230000000000").unwrap().scale, 12);
        assert!(ExactRate::parse("0").is_err());
        assert!(ExactRate::parse("-1").is_err());
        assert!(ExactRate::parse("9999999999999999999").is_err());
        assert!(ExactRate::parse("1.0000000000001").is_err());
        assert_eq!(
            calculate_destination(100, 2, 2, &ExactRate::parse("1.25").unwrap()),
            Ok(125)
        );
        assert_eq!(
            calculate_destination(1, 0, 0, &ExactRate::parse("0.5").unwrap()),
            Ok(1)
        );
        assert_eq!(
            calculate_destination(1, 0, 0, &ExactRate::parse("0.49").unwrap()),
            Err(ArithmeticError::DestinationTooSmall)
        );
        assert_eq!(
            calculate_destination(100, 2, 0, &ExactRate::parse("1.25").unwrap()),
            Ok(1)
        );
        assert_eq!(
            calculate_destination(1, 0, 2, &ExactRate::parse("1.25").unwrap()),
            Ok(125)
        );
        assert_eq!(
            calculate_destination(1, 19, 2, &ExactRate::parse("1").unwrap()),
            Err(ArithmeticError::InvalidScale)
        );
    }
    #[test]
    fn fees_round_half_up_and_check_total() {
        assert_eq!(calculate_fee(1, 5).unwrap().fee_minor, 0);
        assert_eq!(calculate_fee(1, 5_000).unwrap().fee_minor, 1);
        assert_eq!(calculate_fee(1, 10_000).unwrap().fee_minor, 1);
        assert_eq!(calculate_fee(i64::MAX, 1), Err(ArithmeticError::Overflow));
    }
    #[test]
    fn bigint_intermediate_exceeds_i128_but_final_value_fits() {
        let rate = ExactRate::parse("999999999999999999.999999999999").unwrap();
        assert_eq!(
            calculate_destination(1, 18, 18, &rate),
            Ok(1_000_000_000_000_000_000)
        );
        assert_eq!(
            calculate_destination(i64::MAX, 0, 18, &rate),
            Err(ArithmeticError::Overflow)
        );
    }
}

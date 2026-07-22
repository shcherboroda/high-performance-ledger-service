use std::time::Duration;

use axum::http::{HeaderMap, header::HeaderName};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::api_error::AppError;

pub const ACCOUNT_CREATION_OPERATION: &str = "account_creation";
pub const TRANSFER_OPERATION: &str = "transfer";
pub const FX_TRANSFER_OPERATION: &str = "fx_transfer";
pub const REVERSAL_OPERATION: &str = "reversal";
pub const IDEMPOTENCY_KEY_HEADER: HeaderName = HeaderName::from_static("idempotency-key");
const MAX_KEY_LENGTH: usize = 255;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn from_headers(headers: &HeaderMap) -> Result<Self, AppError> {
        let values = headers
            .get_all(&IDEMPOTENCY_KEY_HEADER)
            .iter()
            .collect::<Vec<_>>();
        if values.is_empty() {
            return Err(AppError::validation(
                "missing_idempotency_key",
                "The Idempotency-Key header is required",
            ));
        }
        if values.len() != 1 {
            return Err(AppError::validation(
                "duplicate_idempotency_key",
                "The Idempotency-Key header must appear exactly once",
            ));
        }
        let value = values[0].to_str().map_err(|_| {
            AppError::validation(
                "malformed_idempotency_key",
                "The Idempotency-Key header is invalid",
            )
        })?;
        if value.is_empty()
            || value.len() > MAX_KEY_LENGTH
            || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        {
            return Err(AppError::validation(
                "malformed_idempotency_key",
                "The Idempotency-Key header is invalid",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn account_creation_fingerprint(currency: &str, balance_minor: i64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"account_creation:v1:");
    hasher.update(currency.as_bytes());
    hasher.update(b":");
    hasher.update(balance_minor.to_be_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn transfer_fingerprint(
    source_account_id: Uuid,
    destination_account_id: Uuid,
    currency: &str,
    amount_minor: i64,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"transfer:v1:");
    hasher.update(source_account_id.as_bytes());
    hasher.update(destination_account_id.as_bytes());
    hasher.update(currency.as_bytes());
    hasher.update(amount_minor.to_be_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn reversal_fingerprint(original_transfer_id: Uuid) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"reversal:v1:");
    hasher.update(original_transfer_id.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub enum Reservation {
    Owned,
    Replay {
        http_status: i32,
        response_body: serde_json::Value,
    },
    Conflict,
}

pub async fn completed_success(
    transaction: &mut Transaction<'_, Postgres>,
    client_id: &str,
    operation_type: &str,
    key: &IdempotencyKey,
) -> Result<Option<(String, i32, serde_json::Value)>, sqlx::Error> {
    let record = sqlx::query_as::<_, (String, Option<i32>, Option<serde_json::Value>)>(
        "SELECT request_fingerprint, http_status, response_body \
         FROM idempotency_records WHERE client_id = $1 AND operation_type = $2 AND idempotency_key = $3",
    )
    .bind(client_id)
    .bind(operation_type)
    .bind(key.as_str())
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(record.map(|(fingerprint, http_status, response_body)| {
        let http_status = http_status.expect("incomplete idempotency records cannot commit");
        let response_body = response_body.expect("incomplete idempotency records cannot commit");
        (fingerprint, http_status, response_body)
    }))
}

pub async fn reserve(
    transaction: &mut Transaction<'_, Postgres>,
    client_id: &str,
    operation_type: &str,
    key: &IdempotencyKey,
    fingerprint: &str,
    retention: Duration,
) -> Result<Reservation, sqlx::Error> {
    let retention_seconds =
        i64::try_from(retention.as_secs()).expect("validated retention fits i64");
    let inserted = sqlx::query(
        "INSERT INTO idempotency_records (client_id, operation_type, idempotency_key, request_fingerprint, expires_at) \
         VALUES ($1, $2, $3, $4, now() + ($5 * interval '1 second')) \
         ON CONFLICT DO NOTHING",
    )
    .bind(client_id)
    .bind(operation_type)
    .bind(key.as_str())
    .bind(fingerprint)
    .bind(retention_seconds)
    .execute(&mut **transaction)
    .await?;
    if inserted.rows_affected() == 1 {
        return Ok(Reservation::Owned);
    }

    let record = sqlx::query_as::<_, (String, Option<i32>, Option<serde_json::Value>)>(
        "SELECT request_fingerprint, http_status, response_body \
         FROM idempotency_records WHERE client_id = $1 AND operation_type = $2 AND idempotency_key = $3",
    )
    .bind(client_id)
    .bind(operation_type)
    .bind(key.as_str())
    .fetch_one(&mut **transaction)
    .await?;
    if record.0 != fingerprint {
        return Ok(Reservation::Conflict);
    }
    match (record.1, record.2) {
        (Some(http_status), Some(response_body)) => Ok(Reservation::Replay {
            http_status,
            response_body,
        }),
        _ => unreachable!("an incomplete idempotency reservation cannot be committed"),
    }
}

pub async fn store_success(
    transaction: &mut Transaction<'_, Postgres>,
    client_id: &str,
    operation_type: &str,
    key: &IdempotencyKey,
    http_status: i32,
    response_body: &serde_json::Value,
    resulting_resource_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE idempotency_records SET http_status = $1, response_body = $2, resulting_resource_id = $3 \
         WHERE client_id = $4 AND operation_type = $5 AND idempotency_key = $6",
    )
    .bind(http_status)
    .bind(response_body)
    .bind(resulting_resource_id)
    .bind(client_id)
    .bind(operation_type)
    .bind(key.as_str())
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};
    use sqlx::PgPool;

    use super::*;

    #[test]
    fn validates_idempotency_header() {
        let cases = [
            (None, "missing_idempotency_key"),
            (Some(""), "malformed_idempotency_key"),
            (Some("   "), "malformed_idempotency_key"),
            (Some("contains space"), "malformed_idempotency_key"),
        ];
        for (value, code) in cases {
            let mut headers = HeaderMap::new();
            if let Some(value) = value {
                headers.insert(IDEMPOTENCY_KEY_HEADER, value.parse().unwrap());
            }
            assert!(
                matches!(IdempotencyKey::from_headers(&headers), Err(AppError::Validation { code: actual, .. }) if actual == code)
            );
        }
        let mut duplicate = HeaderMap::new();
        duplicate.append(IDEMPOTENCY_KEY_HEADER, "one".parse().unwrap());
        duplicate.append(IDEMPOTENCY_KEY_HEADER, "two".parse().unwrap());
        assert!(matches!(
            IdempotencyKey::from_headers(&duplicate),
            Err(AppError::Validation {
                code: "duplicate_idempotency_key",
                ..
            })
        ));
        let boundary = "a".repeat(MAX_KEY_LENGTH);
        let mut headers = HeaderMap::new();
        headers.insert(IDEMPOTENCY_KEY_HEADER, boundary.parse().unwrap());
        assert_eq!(
            IdempotencyKey::from_headers(&headers).unwrap().as_str(),
            boundary
        );
        headers.insert(
            IDEMPOTENCY_KEY_HEADER,
            "a".repeat(MAX_KEY_LENGTH + 1).parse().unwrap(),
        );
        assert!(IdempotencyKey::from_headers(&headers).is_err());
        let mut invalid = HeaderMap::new();
        invalid.insert(
            IDEMPOTENCY_KEY_HEADER,
            HeaderValue::from_bytes(b"tab\tkey").unwrap(),
        );
        assert!(IdempotencyKey::from_headers(&invalid).is_err());
    }

    #[test]
    fn account_fingerprint_is_normalized_and_deterministic() {
        let first = account_creation_fingerprint("PLN", 1020);
        assert_eq!(first, account_creation_fingerprint("PLN", 1020));
        assert_ne!(first, account_creation_fingerprint("USD", 1020));
        assert_ne!(first, account_creation_fingerprint("PLN", 1021));
    }

    #[test]
    fn transfer_fingerprint_covers_every_business_input() {
        let source = Uuid::new_v4();
        let destination = Uuid::new_v4();
        let fingerprint = transfer_fingerprint(source, destination, "USD", 1020);
        assert_eq!(
            fingerprint,
            transfer_fingerprint(source, destination, "USD", 1020)
        );
        assert_ne!(
            fingerprint,
            transfer_fingerprint(destination, source, "USD", 1020)
        );
        assert_ne!(
            fingerprint,
            transfer_fingerprint(source, destination, "PLN", 1020)
        );
        assert_ne!(
            fingerprint,
            transfer_fingerprint(source, destination, "USD", 1021)
        );
    }

    #[test]
    fn reversal_fingerprint_covers_the_original_transfer() {
        let original = Uuid::new_v4();
        assert_eq!(
            reversal_fingerprint(original),
            reversal_fingerprint(original)
        );
        assert_ne!(
            reversal_fingerprint(original),
            reversal_fingerprint(Uuid::new_v4())
        );
    }

    #[sqlx::test]
    async fn reservation_rolls_back_on_failure_and_keys_are_scoped_by_operation(pool: PgPool) {
        let mut headers = HeaderMap::new();
        headers.insert(IDEMPOTENCY_KEY_HEADER, "shared-key".parse().unwrap());
        let key = IdempotencyKey::from_headers(&headers).unwrap();
        let fingerprint = account_creation_fingerprint("PLN", 100);

        let mut failed = pool.begin().await.unwrap();
        assert!(matches!(
            reserve(
                &mut failed,
                "client",
                ACCOUNT_CREATION_OPERATION,
                &key,
                &fingerprint,
                Duration::from_secs(60),
            )
            .await
            .unwrap(),
            Reservation::Owned
        ));
        drop(failed);

        let mut retry = pool.begin().await.unwrap();
        assert!(matches!(
            reserve(
                &mut retry,
                "client",
                ACCOUNT_CREATION_OPERATION,
                &key,
                &fingerprint,
                Duration::from_secs(60),
            )
            .await
            .unwrap(),
            Reservation::Owned
        ));
        store_success(
            &mut retry,
            "client",
            ACCOUNT_CREATION_OPERATION,
            &key,
            201,
            &serde_json::json!({"id":"result"}),
            Uuid::new_v4(),
        )
        .await
        .unwrap();
        retry.commit().await.unwrap();

        let mut another_operation = pool.begin().await.unwrap();
        assert!(matches!(
            reserve(
                &mut another_operation,
                "client",
                "another_operation",
                &key,
                &fingerprint,
                Duration::from_secs(60),
            )
            .await
            .unwrap(),
            Reservation::Owned
        ));
    }
}

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Database representation of a transfer visible to one of its participants.
pub(crate) struct TransferDetailsRow {
    pub source_account_id: Uuid,
    pub destination_account_id: Uuid,
    pub source_currency: String,
    pub destination_currency: String,
    pub source_amount_minor: i64,
    pub destination_amount_minor: i64,
    pub fee_amount_minor: i64,
    pub total_source_debit_minor: i64,
    pub fee_bps: Option<i32>,
    pub exchange_rate: Option<String>,
    pub exchange_rate_id: Option<Uuid>,
    pub kind: String,
    pub reverses_transfer_id: Option<Uuid>,
    pub created_at: String,
    pub source_scale: u8,
    pub destination_scale: u8,
}

/// Reads the immutable snapshot only when the caller owns either participant account.
pub(crate) async fn find_details(
    pool: &PgPool,
    transfer_id: Uuid,
    client_id: &str,
) -> Result<Option<TransferDetailsRow>, sqlx::Error> {
    type RawTransferDetailsRow = (
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
    let row = sqlx::query_as::<_, RawTransferDetailsRow>(
        r#"
        SELECT t.source_account_id, t.destination_account_id, t.source_currency, t.destination_currency,
               t.source_amount_minor, t.destination_amount_minor, t.fee_amount_minor,
               t.total_source_debit_minor, t.fee_bps, t.exchange_rate::text, t.exchange_rate_id,
               t.kind::text, t.reverses_transfer_id, t.created_at::text, source.currency_scale,
               destination.currency_scale
        FROM transfers t
        JOIN accounts source ON source.id = t.source_account_id
        JOIN accounts destination ON destination.id = t.destination_account_id
        WHERE t.id = $1 AND (source.owner_id = $2 OR destination.owner_id = $2)
        "#,
    )
    .bind(transfer_id)
    .bind(client_id)
    .fetch_optional(pool)
    .await?;
    row.map(|row| {
        Ok(TransferDetailsRow {
            source_account_id: row.0,
            destination_account_id: row.1,
            source_currency: row.2,
            destination_currency: row.3,
            source_amount_minor: row.4,
            destination_amount_minor: row.5,
            fee_amount_minor: row.6,
            total_source_debit_minor: row.7,
            fee_bps: row.8,
            exchange_rate: row.9,
            exchange_rate_id: row.10,
            kind: row.11,
            reverses_transfer_id: row.12,
            created_at: row.13,
            source_scale: u8::try_from(row.14).map_err(sqlx::Error::decode)?,
            destination_scale: u8::try_from(row.15).map_err(sqlx::Error::decode)?,
        })
    })
    .transpose()
}

/// Immutable currency metadata used to validate and fingerprint a transfer before locking.
pub(crate) struct TransferMetadata {
    pub source_currency: String,
    pub source_scale: u8,
    pub destination_currency: String,
    pub destination_scale: u8,
}

pub(crate) async fn find_metadata(
    connection: &mut PgConnection,
    source_account_id: Uuid,
    destination_account_id: Uuid,
) -> Result<Option<TransferMetadata>, sqlx::Error> {
    let metadata = sqlx::query_as::<_, (String, i16, String, i16)>(
        "SELECT source.currency, source.currency_scale, destination.currency, destination.currency_scale \
         FROM accounts source JOIN accounts destination ON destination.id = $2 WHERE source.id = $1",
    )
    .bind(source_account_id)
    .bind(destination_account_id)
    .fetch_optional(&mut *connection)
    .await?;
    metadata
        .map(
            |(source_currency, source_scale, destination_currency, destination_scale)| {
                Ok(TransferMetadata {
                    source_currency,
                    source_scale: u8::try_from(source_scale).map_err(sqlx::Error::decode)?,
                    destination_currency,
                    destination_scale: u8::try_from(destination_scale)
                        .map_err(sqlx::Error::decode)?,
                })
            },
        )
        .transpose()
}

/// Account state returned while holding locks in UUID order to avoid deadlocks.
pub(crate) struct LockedAccount {
    pub id: Uuid,
    pub owner_id: String,
    pub currency: String,
    pub scale: u8,
    pub balance_minor: i64,
    pub status: String,
}

pub(crate) async fn lock_accounts(
    transaction: &mut Transaction<'_, Postgres>,
    source_account_id: Uuid,
    destination_account_id: Uuid,
) -> Result<Vec<LockedAccount>, sqlx::Error> {
    let accounts = sqlx::query_as::<_, (Uuid, String, String, i16, i64, String)>(
        "SELECT id, owner_id, currency, currency_scale, balance_minor, status::text \
         FROM accounts WHERE id = ANY($1) ORDER BY id FOR UPDATE",
    )
    .bind(vec![source_account_id, destination_account_id])
    .fetch_all(&mut **transaction)
    .await?;
    accounts
        .into_iter()
        .map(|(id, owner_id, currency, scale, balance_minor, status)| {
            Ok(LockedAccount {
                id,
                owner_id,
                currency,
                scale: u8::try_from(scale).map_err(sqlx::Error::decode)?,
                balance_minor,
                status,
            })
        })
        .collect()
}

pub(crate) async fn transaction_time(
    transaction: &mut Transaction<'_, Postgres>,
) -> Result<DateTime<Utc>, sqlx::Error> {
    sqlx::query_scalar("SELECT transaction_timestamp()")
        .fetch_one(&mut **transaction)
        .await
}

/// Immutable transfer row values written atomically with both balance and entry updates.
pub(crate) struct NewTransfer<'a> {
    pub id: Uuid,
    pub source_account_id: Uuid,
    pub destination_account_id: Uuid,
    pub source_currency: &'a str,
    pub destination_currency: &'a str,
    pub source_amount_minor: i64,
    pub destination_amount_minor: i64,
    pub fee_amount_minor: i64,
    pub total_source_debit_minor: i64,
    pub fee_bps: Option<i32>,
    pub exchange_rate: Option<&'a str>,
    pub exchange_rate_id: Option<Uuid>,
    pub kind: &'a str,
    pub initiated_by: &'a str,
}

pub(crate) async fn insert(
    transaction: &mut Transaction<'_, Postgres>,
    transfer: NewTransfer<'_>,
) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO transfers (id, source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, fee_amount_minor, total_source_debit_minor, fee_bps, exchange_rate, exchange_rate_id, kind, initiated_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, CAST($11 AS numeric), $12, $13::transfer_kind, $14) RETURNING created_at::text",
    )
    .bind(transfer.id)
    .bind(transfer.source_account_id)
    .bind(transfer.destination_account_id)
    .bind(transfer.source_currency)
    .bind(transfer.destination_currency)
    .bind(transfer.source_amount_minor)
    .bind(transfer.destination_amount_minor)
    .bind(transfer.fee_amount_minor)
    .bind(transfer.total_source_debit_minor)
    .bind(transfer.fee_bps)
    .bind(transfer.exchange_rate)
    .bind(transfer.exchange_rate_id)
    .bind(transfer.kind)
    .bind(transfer.initiated_by)
    .fetch_one(&mut **transaction)
    .await
}

pub(crate) async fn update_balance(
    transaction: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    balance_minor: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE accounts SET balance_minor = $1, version = version + 1, updated_at = now() WHERE id = $2",
    )
    .bind(balance_minor)
    .bind(account_id)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

pub(crate) struct NewAccountEntry<'a> {
    pub account_id: Uuid,
    pub transfer_id: Uuid,
    pub counterparty_account_id: Uuid,
    pub direction: &'a str,
    pub operation_kind: &'a str,
    pub amount_minor: i64,
    pub currency: &'a str,
    pub principal_amount_minor: Option<i64>,
    pub fee_amount_minor: Option<i64>,
}

pub(crate) async fn insert_account_entry(
    transaction: &mut Transaction<'_, Postgres>,
    entry: NewAccountEntry<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO account_entries (id, account_id, transfer_id, counterparty_account_id, direction, operation_kind, amount_minor, currency, principal_amount_minor, fee_amount_minor) \
         VALUES ($1, $2, $3, $4, $5::entry_direction, $6::transfer_kind, $7, $8, $9, $10)",
    )
    .bind(Uuid::new_v4())
    .bind(entry.account_id)
    .bind(entry.transfer_id)
    .bind(entry.counterparty_account_id)
    .bind(entry.direction)
    .bind(entry.operation_kind)
    .bind(entry.amount_minor)
    .bind(entry.currency)
    .bind(entry.principal_amount_minor)
    .bind(entry.fee_amount_minor)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

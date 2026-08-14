use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Immutable values from the original transfer retained by a reversal.
pub(crate) struct OriginalTransfer {
    pub source_account_id: Uuid,
    pub destination_account_id: Uuid,
    pub source_currency: String,
    pub destination_currency: String,
    pub destination_amount_minor: i64,
    pub fee_amount_minor: i64,
    pub total_source_debit_minor: i64,
    pub fee_bps: Option<i32>,
    pub kind: String,
    pub exchange_rate: Option<String>,
    pub exchange_rate_id: Option<Uuid>,
}

pub(crate) async fn find_original_for_update(
    transaction: &mut Transaction<'_, Postgres>,
    transfer_id: Uuid,
) -> Result<Option<OriginalTransfer>, sqlx::Error> {
    type RawOriginal = (
        Uuid,
        Uuid,
        String,
        String,
        i64,
        i64,
        i64,
        i64,
        Option<i32>,
        String,
        Option<String>,
        Option<Uuid>,
        String,
    );
    let original = sqlx::query_as::<_, RawOriginal>(
        "SELECT source_account_id, destination_account_id, source_currency, destination_currency, \
         source_amount_minor, destination_amount_minor, fee_amount_minor, total_source_debit_minor, \
         fee_bps, kind::text, exchange_rate::text, exchange_rate_id, initiated_by \
         FROM transfers WHERE id = $1 FOR UPDATE",
    )
    .bind(transfer_id)
    .fetch_optional(&mut **transaction)
    .await?;
    Ok(original.map(|row| OriginalTransfer {
        source_account_id: row.0,
        destination_account_id: row.1,
        source_currency: row.2,
        destination_currency: row.3,
        destination_amount_minor: row.5,
        fee_amount_minor: row.6,
        total_source_debit_minor: row.7,
        fee_bps: row.8,
        kind: row.9,
        exchange_rate: row.10,
        exchange_rate_id: row.11,
    }))
}

pub(crate) async fn destination_is_owned_by(
    transaction: &mut Transaction<'_, Postgres>,
    destination_account_id: Uuid,
    client_id: &str,
) -> Result<bool, sqlx::Error> {
    Ok(
        sqlx::query_scalar::<_, String>("SELECT owner_id FROM accounts WHERE id = $1")
            .bind(destination_account_id)
            .fetch_optional(&mut **transaction)
            .await?
            .as_deref()
            == Some(client_id),
    )
}

pub(crate) async fn exists_for_original(
    transaction: &mut Transaction<'_, Postgres>,
    original_transfer_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM transfers WHERE reverses_transfer_id = $1)")
        .bind(original_transfer_id)
        .fetch_one(&mut **transaction)
        .await
}

pub(crate) struct NewReversal<'a> {
    pub id: Uuid,
    pub original_transfer_id: Uuid,
    pub source_account_id: Uuid,
    pub destination_account_id: Uuid,
    pub source_currency: &'a str,
    pub destination_currency: &'a str,
    pub source_amount_minor: i64,
    pub destination_amount_minor: i64,
    pub original_fee_amount_minor: i64,
    pub fee_bps: Option<i32>,
    pub exchange_rate: Option<&'a str>,
    pub exchange_rate_id: Option<Uuid>,
    pub initiated_by: &'a str,
}

pub(crate) async fn insert(
    transaction: &mut Transaction<'_, Postgres>,
    reversal: NewReversal<'_>,
) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO transfers (id, source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, fee_amount_minor, total_source_debit_minor, fee_bps, exchange_rate, exchange_rate_id, kind, reverses_transfer_id, initiated_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $6, $9, CAST($10 AS numeric), $11, 'reversal', $12, $13) RETURNING created_at::text",
    )
    .bind(reversal.id)
    .bind(reversal.source_account_id)
    .bind(reversal.destination_account_id)
    .bind(reversal.source_currency)
    .bind(reversal.destination_currency)
    .bind(reversal.source_amount_minor)
    .bind(reversal.destination_amount_minor)
    .bind(reversal.original_fee_amount_minor)
    .bind(reversal.fee_bps)
    .bind(reversal.exchange_rate)
    .bind(reversal.exchange_rate_id)
    .bind(reversal.original_transfer_id)
    .bind(reversal.initiated_by)
    .fetch_one(&mut **transaction)
    .await
}

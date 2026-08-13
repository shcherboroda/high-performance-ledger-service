use sqlx::PgPool;
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

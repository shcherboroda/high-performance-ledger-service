use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Values written to the authoritative account row.
pub(crate) struct NewAccount<'a> {
    pub id: Uuid,
    pub owner_id: &'a str,
    pub currency: &'a str,
    pub scale: u8,
    pub balance_minor: i64,
}

pub(crate) async fn insert(
    transaction: &mut Transaction<'_, Postgres>,
    account: NewAccount<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO accounts (id, owner_id, currency, currency_scale, balance_minor) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(account.id)
    .bind(account.owner_id)
    .bind(account.currency)
    .bind(i16::from(account.scale))
    .bind(account.balance_minor)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

pub(crate) struct StoredBalance {
    pub currency: String,
    pub scale: u8,
    pub balance_minor: i64,
    pub version: i64,
}

pub(crate) async fn find_balance(
    pool: &PgPool,
    account_id: Uuid,
    owner_id: &str,
) -> Result<Option<StoredBalance>, sqlx::Error> {
    let account = sqlx::query_as::<_, (String, i16, i64, i64)>(
        "SELECT currency, currency_scale, balance_minor, version \
         FROM accounts WHERE id = $1 AND owner_id = $2",
    )
    .bind(account_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await?;
    account
        .map(|(currency, scale, balance_minor, version)| {
            Ok(StoredBalance {
                currency,
                scale: u8::try_from(scale).map_err(sqlx::Error::decode)?,
                balance_minor,
                version,
            })
        })
        .transpose()
}

use chrono::{TimeZone, Utc};
use rust_backend_technical_assessment::fx::{
    ConfigurationError, select_exchange_rate, select_fee_rule,
};
use sqlx::PgPool;
use uuid::Uuid;

async fn rate(pool: &PgPool, source: &str, destination: &str, from: &str, until: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO exchange_rates (id, source_currency, destination_currency, rate, valid_from, valid_until) VALUES ($1, $2, $3, 1.25, $4::timestamptz, $5::timestamptz)")
        .bind(id).bind(source).bind(destination).bind(from).bind(until).execute(pool).await.unwrap();
    id
}
async fn fee(
    pool: &PgPool,
    source: Option<&str>,
    destination: Option<&str>,
    bps: i32,
    from: &str,
    until: &str,
) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO fx_fee_rules (id, source_currency, destination_currency, fee_bps, valid_from, valid_until) VALUES ($1, $2, $3, $4, $5::timestamptz, $6::timestamptz)")
        .bind(id).bind(source).bind(destination).bind(bps).bind(from).bind(until).execute(pool).await.unwrap();
    id
}

#[sqlx::test]
async fn selection_is_directional_fail_closed_and_uses_one_timestamp(pool: PgPool) {
    let operation_time = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
    let direct = rate(
        &pool,
        "EUR",
        "PLN",
        "2030-01-01 00:00:00+00",
        "2030-02-01 00:00:00+00",
    )
    .await;
    rate(
        &pool,
        "PLN",
        "EUR",
        "2029-01-01 00:00:00+00",
        "2031-01-01 00:00:00+00",
    )
    .await;
    let selected = select_exchange_rate(&pool, "EUR", "PLN", operation_time)
        .await
        .unwrap();
    assert_eq!(selected.id, direct);
    assert_eq!(
        select_exchange_rate(&pool, "USD", "EUR", operation_time).await,
        Err(ConfigurationError::RateUnavailable)
    );
    assert_eq!(
        select_exchange_rate(
            &pool,
            "EUR",
            "USD",
            Utc.with_ymd_and_hms(2030, 2, 1, 0, 0, 0).unwrap()
        )
        .await,
        Err(ConfigurationError::RateUnavailable)
    );
    rate(
        &pool,
        "EUR",
        "PLN",
        "2029-01-01 00:00:00+00",
        "2031-01-01 00:00:00+00",
    )
    .await;
    assert_eq!(
        select_exchange_rate(&pool, "EUR", "PLN", operation_time).await,
        Err(ConfigurationError::RateAmbiguous)
    );
}

#[sqlx::test]
async fn fee_rules_prioritize_pairs_and_fail_closed_at_each_level(pool: PgPool) {
    let at = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
    let default = fee(
        &pool,
        None,
        None,
        100,
        "2029-01-01 00:00:00+00",
        "2031-01-01 00:00:00+00",
    )
    .await;
    assert_eq!(
        select_fee_rule(&pool, "EUR", "PLN", at).await.unwrap().id,
        default
    );
    let pair = fee(
        &pool,
        Some("EUR"),
        Some("PLN"),
        200,
        "2029-01-01 00:00:00+00",
        "2031-01-01 00:00:00+00",
    )
    .await;
    assert_eq!(
        select_fee_rule(&pool, "EUR", "PLN", at).await.unwrap().id,
        pair
    );
    fee(
        &pool,
        Some("EUR"),
        Some("PLN"),
        300,
        "2029-01-01 00:00:00+00",
        "2031-01-01 00:00:00+00",
    )
    .await;
    assert_eq!(
        select_fee_rule(&pool, "EUR", "PLN", at).await,
        Err(ConfigurationError::FeeRuleAmbiguous)
    );
}

#[sqlx::test]
async fn fx_schema_constrains_rows_and_retains_referenced_rates(pool: PgPool) -> sqlx::Result<()> {
    for statement in [
        "INSERT INTO exchange_rates (id, source_currency, destination_currency, rate, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000001', 'eur', 'PLN', 1, now(), now() + interval '1 hour')",
        "INSERT INTO exchange_rates (id, source_currency, destination_currency, rate, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000002', 'EUR', 'EUR', 1, now(), now() + interval '1 hour')",
        "INSERT INTO fx_fee_rules (id, source_currency, destination_currency, fee_bps, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000003', 'EUR', NULL, 1, now(), now() + interval '1 hour')",
        "INSERT INTO fx_fee_rules (id, fee_bps, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000004', 10001, now(), now() + interval '1 hour')",
    ] {
        assert!(sqlx::query(statement).execute(&pool).await.is_err());
    }
    let rate_id = rate(
        &pool,
        "EUR",
        "PLN",
        "2029-01-01 00:00:00+00",
        "2031-01-01 00:00:00+00",
    )
    .await;
    let source = Uuid::new_v4();
    let destination = Uuid::new_v4();
    let transfer = Uuid::new_v4();
    for id in [source, destination] {
        sqlx::query("INSERT INTO accounts (id, owner_id, currency, currency_scale, balance_minor) VALUES ($1, 'owner', 'EUR', 2, 0)").bind(id).execute(&pool).await?;
    }
    sqlx::query("INSERT INTO transfers (id, source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, total_source_debit_minor, exchange_rate_id, kind, initiated_by) VALUES ($1, $2, $3, 'EUR', 'PLN', 1, 1, 1, $4, 'fx_transfer', 'owner')").bind(transfer).bind(source).bind(destination).bind(rate_id).execute(&pool).await?;
    assert!(
        sqlx::query("DELETE FROM exchange_rates WHERE id = $1")
            .bind(rate_id)
            .execute(&pool)
            .await
            .is_err()
    );
    sqlx::query("DELETE FROM transfers WHERE id = $1")
        .bind(transfer)
        .execute(&pool)
        .await?;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM exchange_rates WHERE id = $1")
            .bind(rate_id)
            .fetch_one(&pool)
            .await?,
        1
    );
    Ok(())
}

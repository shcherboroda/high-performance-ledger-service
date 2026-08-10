use chrono::{TimeZone, Utc};
use ledger_service::fx::{ConfigurationError, select_exchange_rate, select_fee_rule};
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
async fn selectors_use_the_callers_transaction_and_fail_closed(pool: PgPool) -> sqlx::Result<()> {
    let direct = rate(
        &pool,
        "EUR",
        "PLN",
        "2020-01-01 00:00:00+00",
        "2030-01-01 00:00:00+00",
    )
    .await;
    rate(
        &pool,
        "PLN",
        "EUR",
        "2020-01-01 00:00:00+00",
        "2030-01-01 00:00:00+00",
    )
    .await;
    let default = fee(
        &pool,
        None,
        None,
        100,
        "2020-01-01 00:00:00+00",
        "2030-01-01 00:00:00+00",
    )
    .await;
    let pair = fee(
        &pool,
        Some("EUR"),
        Some("PLN"),
        200,
        "2020-01-01 00:00:00+00",
        "2030-01-01 00:00:00+00",
    )
    .await;
    let mut transaction = pool.begin().await?;
    let operation_time =
        sqlx::query_scalar::<_, chrono::DateTime<Utc>>("SELECT transaction_timestamp()")
            .fetch_one(&mut *transaction)
            .await?;
    assert_eq!(
        select_exchange_rate(&mut transaction, "EUR", "PLN", operation_time)
            .await
            .unwrap()
            .id,
        direct
    );
    assert_eq!(
        select_fee_rule(&mut transaction, "EUR", "PLN", operation_time)
            .await
            .unwrap()
            .id,
        pair
    );
    assert_eq!(
        select_fee_rule(&mut transaction, "USD", "JPY", operation_time)
            .await
            .unwrap()
            .id,
        default
    );
    assert!(matches!(
        select_exchange_rate(&mut transaction, "USD", "EUR", operation_time).await,
        Err(ConfigurationError::RateUnavailable)
    ));
    assert!(
        select_fee_rule(&mut transaction, "PLN", "USD", operation_time)
            .await
            .is_ok()
    );
    transaction.rollback().await
}

#[sqlx::test]
async fn selectors_enforce_direction_multiplicity_and_validity_boundaries(
    pool: PgPool,
) -> sqlx::Result<()> {
    let from = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
    let until = Utc.with_ymd_and_hms(2030, 1, 2, 0, 0, 0).unwrap();
    rate(
        &pool,
        "EUR",
        "PLN",
        "2030-01-01 00:00:00+00",
        "2030-01-02 00:00:00+00",
    )
    .await;
    rate(
        &pool,
        "EUR",
        "PLN",
        "2020-01-01 00:00:00+00",
        "2021-01-01 00:00:00+00",
    )
    .await;
    rate(
        &pool,
        "EUR",
        "PLN",
        "2040-01-01 00:00:00+00",
        "2041-01-01 00:00:00+00",
    )
    .await;
    fee(
        &pool,
        Some("EUR"),
        Some("PLN"),
        1,
        "2030-01-01 00:00:00+00",
        "2030-01-02 00:00:00+00",
    )
    .await;
    let mut connection = pool.acquire().await?;
    assert!(
        select_exchange_rate(&mut connection, "EUR", "PLN", from)
            .await
            .is_ok()
    );
    assert!(
        select_exchange_rate(
            &mut connection,
            "EUR",
            "PLN",
            until - chrono::Duration::microseconds(1)
        )
        .await
        .is_ok()
    );
    assert!(matches!(
        select_exchange_rate(&mut connection, "EUR", "PLN", until).await,
        Err(ConfigurationError::RateUnavailable)
    ));
    assert!(
        select_fee_rule(&mut connection, "EUR", "PLN", from)
            .await
            .is_ok()
    );
    assert!(
        select_fee_rule(
            &mut connection,
            "EUR",
            "PLN",
            until - chrono::Duration::microseconds(1)
        )
        .await
        .is_ok()
    );
    assert!(matches!(
        select_fee_rule(&mut connection, "EUR", "PLN", until).await,
        Err(ConfigurationError::FeeRuleUnavailable)
    ));
    drop(connection);
    rate(
        &pool,
        "EUR",
        "PLN",
        "2029-01-01 00:00:00+00",
        "2031-01-01 00:00:00+00",
    )
    .await;
    rate(
        &pool,
        "EUR",
        "PLN",
        "2029-02-01 00:00:00+00",
        "2031-01-01 00:00:00+00",
    )
    .await;
    rate(
        &pool,
        "EUR",
        "PLN",
        "2029-03-01 00:00:00+00",
        "2031-01-01 00:00:00+00",
    )
    .await;
    let mut connection = pool.acquire().await?;
    assert!(matches!(
        select_exchange_rate(&mut connection, "EUR", "PLN", from).await,
        Err(ConfigurationError::RateAmbiguous)
    ));
    Ok(())
}

#[sqlx::test]
async fn fee_selection_prioritizes_pairs_and_detects_default_ambiguity(
    pool: PgPool,
) -> sqlx::Result<()> {
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
    let mut connection = pool.acquire().await?;
    assert_eq!(
        select_fee_rule(&mut connection, "EUR", "PLN", at)
            .await
            .unwrap()
            .id,
        default
    );
    drop(connection);
    let pair = fee(
        &pool,
        Some("EUR"),
        Some("PLN"),
        200,
        "2029-01-01 00:00:00+00",
        "2031-01-01 00:00:00+00",
    )
    .await;
    let mut connection = pool.acquire().await?;
    assert_eq!(
        select_fee_rule(&mut connection, "EUR", "PLN", at)
            .await
            .unwrap()
            .id,
        pair
    );
    drop(connection);
    fee(
        &pool,
        Some("EUR"),
        Some("PLN"),
        300,
        "2029-01-01 00:00:00+00",
        "2031-01-01 00:00:00+00",
    )
    .await;
    let mut connection = pool.acquire().await?;
    assert!(matches!(
        select_fee_rule(&mut connection, "EUR", "PLN", at).await,
        Err(ConfigurationError::FeeRuleAmbiguous)
    ));
    drop(connection);
    fee(
        &pool,
        None,
        None,
        150,
        "2029-01-01 00:00:00+00",
        "2031-01-01 00:00:00+00",
    )
    .await;
    let mut connection = pool.acquire().await?;
    assert!(matches!(
        select_fee_rule(&mut connection, "USD", "JPY", at).await,
        Err(ConfigurationError::FeeRuleAmbiguous)
    ));
    Ok(())
}

#[sqlx::test]
async fn fee_selection_ignores_non_applicable_rules_and_detects_many_overlaps(
    pool: PgPool,
) -> sqlx::Result<()> {
    let at = Utc.with_ymd_and_hms(2030, 1, 1, 0, 0, 0).unwrap();
    let mut connection = pool.acquire().await?;
    assert!(matches!(
        select_fee_rule(&mut connection, "EUR", "PLN", at).await,
        Err(ConfigurationError::FeeRuleUnavailable)
    ));
    drop(connection);
    for (source, destination, from, until) in [
        (
            Some("PLN"),
            Some("EUR"),
            "2029-01-01 00:00:00+00",
            "2031-01-01 00:00:00+00",
        ),
        (
            Some("EUR"),
            Some("PLN"),
            "2020-01-01 00:00:00+00",
            "2021-01-01 00:00:00+00",
        ),
        (
            Some("EUR"),
            Some("PLN"),
            "2040-01-01 00:00:00+00",
            "2041-01-01 00:00:00+00",
        ),
        (
            None,
            None,
            "2020-01-01 00:00:00+00",
            "2021-01-01 00:00:00+00",
        ),
        (
            None,
            None,
            "2040-01-01 00:00:00+00",
            "2041-01-01 00:00:00+00",
        ),
    ] {
        fee(&pool, source, destination, 100, from, until).await;
    }
    let mut connection = pool.acquire().await?;
    assert!(matches!(
        select_fee_rule(&mut connection, "EUR", "PLN", at).await,
        Err(ConfigurationError::FeeRuleUnavailable)
    ));
    drop(connection);
    for month in 1..=3 {
        fee(
            &pool,
            Some("EUR"),
            Some("PLN"),
            100,
            &format!("2029-{month:02}-01 00:00:00+00"),
            "2031-01-01 00:00:00+00",
        )
        .await;
    }
    let mut connection = pool.acquire().await?;
    assert!(matches!(
        select_fee_rule(&mut connection, "EUR", "PLN", at).await,
        Err(ConfigurationError::FeeRuleAmbiguous)
    ));
    drop(connection);
    for month in 1..=3 {
        fee(
            &pool,
            None,
            None,
            100,
            &format!("2029-{month:02}-01 00:00:00+00"),
            "2031-01-01 00:00:00+00",
        )
        .await;
    }
    let mut connection = pool.acquire().await?;
    assert!(matches!(
        select_fee_rule(&mut connection, "USD", "JPY", at).await,
        Err(ConfigurationError::FeeRuleAmbiguous)
    ));
    Ok(())
}

#[sqlx::test]
async fn fx_schema_constraints_indexes_and_rate_references_hold(pool: PgPool) -> sqlx::Result<()> {
    for statement in [
        "INSERT INTO exchange_rates (id, source_currency, destination_currency, rate, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000001', 'eur', 'PLN', 1, now(), now() + interval '1 hour')",
        "INSERT INTO exchange_rates (id, source_currency, destination_currency, rate, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000002', 'EUR', 'pln', 1, now(), now() + interval '1 hour')",
        "INSERT INTO exchange_rates (id, source_currency, destination_currency, rate, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000003', 'EUR', 'EUR', 1, now(), now() + interval '1 hour')",
        "INSERT INTO exchange_rates (id, source_currency, destination_currency, rate, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000004', 'EUR', 'PLN', 0, now(), now() + interval '1 hour')",
        "INSERT INTO exchange_rates (id, source_currency, destination_currency, rate, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000005', 'EUR', 'PLN', -1, now(), now() + interval '1 hour')",
        "INSERT INTO exchange_rates (id, source_currency, destination_currency, rate, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000006', 'EUR', 'PLN', 1, now(), now())",
        "INSERT INTO exchange_rates (id, source_currency, destination_currency, rate, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000007', 'EUR', 'PLN', 1, now(), now() - interval '1 hour')",
        "INSERT INTO fx_fee_rules (id, source_currency, destination_currency, fee_bps, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000008', NULL, 'PLN', 1, now(), now() + interval '1 hour')",
        "INSERT INTO fx_fee_rules (id, source_currency, destination_currency, fee_bps, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000009', 'EUR', NULL, 1, now(), now() + interval '1 hour')",
        "INSERT INTO fx_fee_rules (id, source_currency, destination_currency, fee_bps, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000010', 'eur', 'PLN', 1, now(), now() + interval '1 hour')",
        "INSERT INTO fx_fee_rules (id, source_currency, destination_currency, fee_bps, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000011', 'EUR', 'pln', 1, now(), now() + interval '1 hour')",
        "INSERT INTO fx_fee_rules (id, source_currency, destination_currency, fee_bps, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000012', 'EUR', 'EUR', 1, now(), now() + interval '1 hour')",
        "INSERT INTO fx_fee_rules (id, source_currency, destination_currency, fee_bps, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000013', 'EUR', 'PLN', -1, now(), now() + interval '1 hour')",
        "INSERT INTO fx_fee_rules (id, source_currency, destination_currency, fee_bps, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000014', 'EUR', 'PLN', 10001, now(), now() + interval '1 hour')",
        "INSERT INTO fx_fee_rules (id, source_currency, destination_currency, fee_bps, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000015', 'EUR', 'PLN', 1, now(), now())",
        "INSERT INTO fx_fee_rules (id, source_currency, destination_currency, fee_bps, valid_from, valid_until) VALUES ('10000000-0000-0000-0000-000000000016', 'EUR', 'PLN', 1, now(), now() - interval '1 hour')",
    ] {
        assert!(sqlx::query(statement).execute(&pool).await.is_err());
    }
    for (name, columns, predicate) in [
        (
            "exchange_rates_directional_validity",
            "source_currency, destination_currency, valid_from, valid_until",
            None,
        ),
        (
            "fx_fee_rules_pair_validity",
            "source_currency, destination_currency, valid_from, valid_until",
            Some("source_currency IS NOT NULL"),
        ),
        (
            "fx_fee_rules_default_validity",
            "valid_from, valid_until",
            Some("source_currency IS NULL"),
        ),
    ] {
        let definition: String =
            sqlx::query_scalar("SELECT indexdef FROM pg_indexes WHERE indexname = $1")
                .bind(name)
                .fetch_one(&pool)
                .await?;
        assert!(definition.contains(columns), "{definition}");
        if let Some(predicate) = predicate {
            assert!(definition.contains(predicate), "{definition}");
        }
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
    let insert = "INSERT INTO transfers (id, source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, total_source_debit_minor, exchange_rate_id, kind, initiated_by) VALUES ($1, $2, $3, 'EUR', 'PLN', 1, 1, 1, $4, 'fx_transfer', 'owner')";
    sqlx::query(insert)
        .bind(transfer)
        .bind(source)
        .bind(destination)
        .bind(rate_id)
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO transfers (id, source_account_id, destination_account_id, source_currency, destination_currency, source_amount_minor, destination_amount_minor, total_source_debit_minor, kind, initiated_by) VALUES ($1, $2, $3, 'EUR', 'EUR', 1, 1, 1, 'transfer', 'owner')")
        .bind(Uuid::new_v4())
        .bind(source)
        .bind(destination)
        .execute(&pool)
        .await?;
    assert!(
        sqlx::query("DELETE FROM exchange_rates WHERE id = $1")
            .bind(rate_id)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query(insert)
            .bind(Uuid::new_v4())
            .bind(source)
            .bind(destination)
            .bind(Uuid::new_v4())
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

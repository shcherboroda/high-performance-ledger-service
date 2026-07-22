CREATE TABLE exchange_rates (
    id UUID PRIMARY KEY,
    source_currency CHAR(3) NOT NULL CHECK (source_currency::TEXT ~ '^[A-Z]{3}$'),
    destination_currency CHAR(3) NOT NULL CHECK (destination_currency::TEXT ~ '^[A-Z]{3}$'),
    rate NUMERIC(30, 12) NOT NULL CHECK (rate > 0),
    valid_from TIMESTAMPTZ NOT NULL,
    valid_until TIMESTAMPTZ NOT NULL,
    external_reference TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (source_currency <> destination_currency),
    CHECK (valid_until > valid_from)
);

CREATE INDEX exchange_rates_directional_validity
    ON exchange_rates (source_currency, destination_currency, valid_from, valid_until);

CREATE TABLE fx_fee_rules (
    id UUID PRIMARY KEY,
    source_currency CHAR(3),
    destination_currency CHAR(3),
    fee_bps INTEGER NOT NULL CHECK (fee_bps BETWEEN 0 AND 10000),
    valid_from TIMESTAMPTZ NOT NULL,
    valid_until TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((source_currency IS NULL) = (destination_currency IS NULL)),
    CHECK (source_currency IS NULL OR source_currency::TEXT ~ '^[A-Z]{3}$'),
    CHECK (destination_currency IS NULL OR destination_currency::TEXT ~ '^[A-Z]{3}$'),
    CHECK (source_currency IS NULL OR source_currency <> destination_currency),
    CHECK (valid_until > valid_from)
);

CREATE INDEX fx_fee_rules_pair_validity
    ON fx_fee_rules (source_currency, destination_currency, valid_from, valid_until)
    WHERE source_currency IS NOT NULL AND destination_currency IS NOT NULL;

CREATE INDEX fx_fee_rules_default_validity
    ON fx_fee_rules (valid_from, valid_until)
    WHERE source_currency IS NULL AND destination_currency IS NULL;

ALTER TABLE transfers
    ADD CONSTRAINT transfers_exchange_rate_id_fkey
    FOREIGN KEY (exchange_rate_id) REFERENCES exchange_rates(id) ON DELETE RESTRICT ON UPDATE RESTRICT;

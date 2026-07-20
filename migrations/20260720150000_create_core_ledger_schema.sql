CREATE TYPE account_status AS ENUM ('active');

CREATE TYPE transfer_kind AS ENUM ('transfer', 'fx_transfer', 'reversal');

CREATE TYPE entry_direction AS ENUM ('debit', 'credit');

CREATE TABLE accounts (
    id UUID PRIMARY KEY,
    owner_id TEXT NOT NULL CHECK (owner_id ~ '[^[:space:]]'),
    currency CHAR(3) NOT NULL CHECK (currency::TEXT ~ '^[A-Z]{3}$'),
    currency_scale SMALLINT NOT NULL CHECK (currency_scale BETWEEN 0 AND 18),
    balance_minor BIGINT NOT NULL,
    status account_status NOT NULL DEFAULT 'active',
    version BIGINT NOT NULL DEFAULT 0 CHECK (version >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE transfers (
    id UUID PRIMARY KEY,
    source_account_id UUID NOT NULL REFERENCES accounts(id),
    destination_account_id UUID NOT NULL REFERENCES accounts(id),
    source_currency CHAR(3) NOT NULL CHECK (source_currency::TEXT ~ '^[A-Z]{3}$'),
    destination_currency CHAR(3) NOT NULL CHECK (destination_currency::TEXT ~ '^[A-Z]{3}$'),
    source_amount_minor BIGINT NOT NULL CHECK (source_amount_minor > 0),
    destination_amount_minor BIGINT NOT NULL CHECK (destination_amount_minor > 0),
    fee_amount_minor BIGINT NOT NULL DEFAULT 0 CHECK (fee_amount_minor >= 0),
    total_source_debit_minor BIGINT NOT NULL CHECK (total_source_debit_minor > 0),
    fee_bps INTEGER,
    exchange_rate NUMERIC(30, 12),
    exchange_rate_id UUID,
    kind transfer_kind NOT NULL,
    reverses_transfer_id UUID REFERENCES transfers(id),
    initiated_by TEXT NOT NULL CHECK (initiated_by ~ '[^[:space:]]'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (source_account_id <> destination_account_id),
    CHECK (reverses_transfer_id IS NULL OR reverses_transfer_id <> id)
);

CREATE UNIQUE INDEX transfers_single_reversal
    ON transfers (reverses_transfer_id)
    WHERE reverses_transfer_id IS NOT NULL;

CREATE TABLE account_entries (
    id UUID PRIMARY KEY,
    account_id UUID NOT NULL REFERENCES accounts(id),
    transfer_id UUID NOT NULL REFERENCES transfers(id),
    counterparty_account_id UUID NOT NULL REFERENCES accounts(id),
    direction entry_direction NOT NULL,
    operation_kind transfer_kind NOT NULL,
    amount_minor BIGINT NOT NULL CHECK (amount_minor > 0),
    currency CHAR(3) NOT NULL CHECK (currency::TEXT ~ '^[A-Z]{3}$'),
    principal_amount_minor BIGINT CHECK (principal_amount_minor IS NULL OR principal_amount_minor > 0),
    fee_amount_minor BIGINT CHECK (fee_amount_minor IS NULL OR fee_amount_minor >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (account_id <> counterparty_account_id)
);

CREATE INDEX account_entries_history
    ON account_entries (account_id, created_at DESC, id DESC);

CREATE INDEX account_entries_pair_history
    ON account_entries (account_id, counterparty_account_id, created_at DESC, id DESC);

CREATE TABLE idempotency_records (
    client_id TEXT NOT NULL CHECK (client_id ~ '[^[:space:]]'),
    operation_type TEXT NOT NULL CHECK (operation_type ~ '[^[:space:]]'),
    idempotency_key TEXT NOT NULL CHECK (idempotency_key ~ '[^[:space:]]'),
    request_fingerprint TEXT NOT NULL CHECK (request_fingerprint ~ '[^[:space:]]'),
    http_status INTEGER,
    response_body JSONB,
    resulting_resource_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (client_id, operation_type, idempotency_key),
    CHECK (expires_at > created_at),
    CHECK (
        (http_status IS NULL AND response_body IS NULL AND resulting_resource_id IS NULL)
        OR (http_status IS NOT NULL AND response_body IS NOT NULL AND resulting_resource_id IS NOT NULL)
    ),
    CHECK (http_status IS NULL OR http_status BETWEEN 200 AND 299)
);

CREATE INDEX idempotency_records_expiry ON idempotency_records (expires_at);

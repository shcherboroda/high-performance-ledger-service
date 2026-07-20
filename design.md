# Ledger Service Design

## 1. Overview

The Ledger Service provides authenticated REST APIs for account management, atomic money transfers, transfer reversal and transaction history.

The service is designed for:

* strong consistency of balances;
* safe concurrent execution;
* low latency on the critical transfer path;
* horizontal scaling of stateless application instances;
* an immutable transfer audit trail;
* efficient account-centric history reads;
* optional cross-currency transfers.

PostgreSQL is the authoritative source of financial state and provides transaction isolation, row-level locking and cross-instance concurrency control.

## 2. Goals

The implementation must:

* create accounts with an initial balance and currency;
* return the current account balance;
* transfer funds atomically between distinct accounts;
* support transfers between accounts owned by the same client;
* prevent overdrafts for normal transfers;
* reject normal transfers between incompatible currencies;
* reverse completed transfers atomically;
* allow an overdraft on the original destination during reversal;
* expose account and account-pair transaction history;
* authenticate all business operations using JWT;
* provide request idempotency;
* return structured errors;
* expose an OpenAPI 3.1 specification;
* remain correct with multiple application instances.

## 3. Non-goals

The following are outside the required scope:

* user registration and credential management;
* JWT issuance and refresh-token handling;
* distributed databases;
* distributed transactions across external services;
* shared or jointly owned accounts;
* configurable role-based access control;
* account status transition endpoints;
* real-time settlement with external financial institutions.

## 4. System architecture

```text
Clients
   |
   | HTTPS + JWT
   v
Load Balancer
   |
   +-------------------+
   |                   |
   v                   v
Ledger Instance A   Ledger Instance B
   |                   |
   +---------+---------+
             |
             v
         PostgreSQL
```

Ledger application instances are stateless. Any instance may process a request for any client.

No application instance owns an account, client or balance. All authoritative state is stored in PostgreSQL.

The application consists of the following logical components:

* HTTP API;
* JWT authentication middleware;
* account application service;
* transfer application service;
* history query service;
* idempotency handling;
* SQLx persistence layer;
* configuration and observability.

HTTP handlers perform transport-level validation and response mapping. Application services own use-case logic and transaction boundaries. SQLx persistence functions execute database operations.

## 5. Authentication and authorization

All business endpoints require an access token:

```http
Authorization: Bearer <JWT>
```

JWT issuance is delegated to an external authentication provider.

The Ledger Service validates:

* token signature;
* expiration time (`exp`);
* subject (`sub`);
* configured issuer (`iss`);
* configured audience (`aud`).

The `sub` claim is used as the authenticated client identifier. JWT verification occurs once at the beginning of every protected HTTP request.

Authorization rules:

* an account is owned by the authenticated client that created it;
* only the owner may read an account balance or history;
* the authenticated client must own the source account of a normal or FX transfer;
* the destination account may belong to the same or another client;
* a reversal may be requested only by the owner of the original destination account.

Health and readiness endpoints do not require JWT. Metrics access is deployment-specific and should normally be restricted at the infrastructure level.

## 6. Domain model

### Client

A client is identified by the JWT `sub` claim. No local client table is required because the service does not manage client profiles or credentials.

### Account

An account has:

* a unique account ID;
* one owner;
* one currency;
* a currency scale;
* a current balance;
* a status;
* a monotonic version;
* creation and update timestamps.

A client may own multiple accounts, including multiple accounts in the same currency. Transfers between a client's own distinct accounts are valid.

The current implementation supports only the `active` status. Both source and destination accounts must exist and be active for transfers and reversals. Future states such as `blocked`, `suspended` and `closed` may be introduced later without changing account identity.

### Money

Balances and persisted monetary amounts are stored as signed 64-bit integers in the smallest supported currency unit.

Examples:

* `10.25 PLN` is stored as `1025` for scale 2;
* `100 JPY` is stored as `100` for scale 0.

Binary floating-point types are not used for balances, fees, rates or transfer amounts.

REST monetary input is represented as a decimal string, for example:

```json
{
  "amount": "10.25"
}
```

The value must have no more fractional digits than the configured currency scale. The API does not silently round normal monetary input.

Account creation requires `initial_balance >= 0`. Transfer and reversal amounts must be greater than zero. Arithmetic overflow is rejected.

### Currency

Currency codes are normalized uppercase three-letter ASCII codes. Supported currencies and scales are defined by a server-side allowlist.

An account's currency and scale are immutable after creation.

### Transfer

A transfer is the immutable authoritative record of one completed business operation.

A transfer stores:

* source and destination accounts;
* source and destination currencies;
* source and destination amounts;
* conversion fee and total source debit;
* applied exchange rate and rate reference when applicable;
* initiating client;
* transfer kind;
* reference to the original transfer for reversals;
* creation timestamp.

Failed transfer attempts do not create completed transfer records.

### Account entry

An account entry is an immutable account-centric history projection. One completed operation creates one entry for each participating account:

* a debit entry for the source account;
* a credit entry for the destination account.

This does not duplicate operations in an account history query because every query is scoped to one selected `account_id`.

Account entries contain only information needed for efficient history lists:

* account ID;
* transfer ID;
* counterparty account ID;
* direction;
* amount and currency relevant to the selected account;
* operation kind;
* principal and fee summary when useful for an outgoing FX operation;
* creation timestamp.

Detailed FX information, the applied rate and reversal relationships remain in `transfers` and are returned by the transfer-details endpoint.

Creating an account does not create a transfer or account entry. The `accounts` row and its `created_at` value are sufficient to represent account creation and its initial balance.

## 7. Data model

The SQL below is illustrative. Exact names may be refined during implementation while preserving the invariants.

### accounts

```sql
CREATE TYPE account_status AS ENUM ('active');

CREATE TABLE accounts (
    id UUID PRIMARY KEY,
    owner_id TEXT NOT NULL,
    currency CHAR(3) NOT NULL,
    currency_scale SMALLINT NOT NULL,
    balance_minor BIGINT NOT NULL,
    status account_status NOT NULL DEFAULT 'active',
    version BIGINT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (currency_scale >= 0)
);
```

The current balance is stored directly in the account row for constant-size balance reads. It is not recalculated from history.

A negative balance is not generally forbidden by a database check because a reversal is explicitly allowed to make the original destination negative. Application transaction logic enforces that normal outgoing transfers cannot create or deepen an overdraft.

### transfers

```sql
CREATE TYPE transfer_kind AS ENUM (
    'transfer',
    'fx_transfer',
    'reversal'
);

CREATE TABLE transfers (
    id UUID PRIMARY KEY,

    source_account_id UUID NOT NULL REFERENCES accounts(id),
    destination_account_id UUID NOT NULL REFERENCES accounts(id),

    source_currency CHAR(3) NOT NULL,
    destination_currency CHAR(3) NOT NULL,

    source_amount_minor BIGINT NOT NULL,
    destination_amount_minor BIGINT NOT NULL,
    fee_amount_minor BIGINT NOT NULL DEFAULT 0,
    total_source_debit_minor BIGINT NOT NULL,

    fee_bps INTEGER,
    exchange_rate NUMERIC(30, 12),
    exchange_rate_id UUID,

    kind transfer_kind NOT NULL,
    reverses_transfer_id UUID REFERENCES transfers(id),

    initiated_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),

    CHECK (source_account_id <> destination_account_id),
    CHECK (source_amount_minor > 0),
    CHECK (destination_amount_minor > 0),
    CHECK (fee_amount_minor >= 0),
    CHECK (total_source_debit_minor > 0)
);
```

A partial unique index prevents multiple reversals of one transfer:

```sql
CREATE UNIQUE INDEX transfers_single_reversal
ON transfers(reverses_transfer_id)
WHERE reverses_transfer_id IS NOT NULL;
```

### account_entries

```sql
CREATE TYPE entry_direction AS ENUM ('debit', 'credit');

CREATE TABLE account_entries (
    id UUID PRIMARY KEY,
    account_id UUID NOT NULL REFERENCES accounts(id),
    transfer_id UUID NOT NULL REFERENCES transfers(id),
    counterparty_account_id UUID NOT NULL REFERENCES accounts(id),
    direction entry_direction NOT NULL,
    operation_kind transfer_kind NOT NULL,
    amount_minor BIGINT NOT NULL,
    currency CHAR(3) NOT NULL,
    principal_amount_minor BIGINT,
    fee_amount_minor BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (amount_minor > 0),
    CHECK (fee_amount_minor IS NULL OR fee_amount_minor >= 0)
);
```

The primary history index is:

```sql
CREATE INDEX account_entries_history
ON account_entries(account_id, created_at DESC, id DESC);
```

Account-pair history additionally uses `counterparty_account_id`:

```sql
CREATE INDEX account_entries_pair_history
ON account_entries(account_id, counterparty_account_id, created_at DESC, id DESC);
```

### idempotency_records

Idempotency records store enough information to replay both successful results and deterministic business errors:

* authenticated client ID;
* idempotency key;
* operation type;
* normalized request fingerprint;
* state;
* HTTP status;
* response body or structured result;
* optional resulting transfer ID;
* creation and expiration timestamps.

The key is unique within the chosen scope, normally `(client_id, operation_type, idempotency_key)`.

## 8. Account creation

Account creation:

1. authenticates the client;
2. validates the currency against the allowlist;
3. parses the initial balance using the currency scale;
4. rejects a negative initial balance;
5. inserts one active `accounts` row;
6. completes the idempotency record where account creation is idempotent;
7. commits.

No `transfer` or `account_entry` is created for the initial balance.

## 9. Atomic transfer algorithm

A normal same-currency transfer is executed in one PostgreSQL transaction.

1. Validate request format and parse the positive decimal-string amount.
2. Resolve or create the idempotency record.
3. Return a stored response for an identical completed request.
4. Reject the same key with a different fingerprint using `409 Conflict`.
5. Start the financial database transaction.
6. Lock both account rows using `SELECT ... FOR UPDATE` in deterministic account-ID order.
7. Verify that both accounts exist and are `active`.
8. Verify that source and destination IDs differ.
9. Verify that the authenticated client owns the source account.
10. Verify that both accounts use the same currency.
11. Verify sufficient source balance.
12. Debit the source and credit the destination.
13. Increment both account versions.
14. Insert the immutable transfer record.
15. Insert the source debit and destination credit account entries.
16. Complete the idempotency record with the success response.
17. Commit.

Both account rows should be selected and locked in one database round trip where practical. The transaction contains no external network calls, event publishing or expensive computation.

## 10. FX transfer

The optional FX operation uses a client-supplied source amount. The system calculates the destination amount.

The authoritative formulas are:

```text
fee = round_half_up(source_amount * fee_bps / 10_000)
total_source_debit = source_amount + fee
destination_amount = round_half_up(source_amount * exchange_rate)
```

The destination amount is rounded half-up to the destination currency scale. Commission is calculated in the source currency and may be zero.

The balance check uses `total_source_debit`.

The applied rate, rate record, fee basis points, fee amount, source amount, destination amount and total debit are captured immutably in the transfer record.

The rate is read and validated before account locks where possible, then revalidated or protected by its validity semantics inside the operation. No external rate-provider call occurs while account rows are locked.

One FX transfer still creates only two account entries. There is no separate fee entry:

* the source entry represents the total debit and may include principal and fee summary fields;
* the destination entry represents the received destination amount.

## 11. Transfer reversal

A reversal is a full, one-time financial inverse of a completed transfer. Partial reversals and reversal of a reversal are not supported.

Only the owner of the original destination account may request reversal.

The reversal transaction:

1. resolves idempotency;
2. locks the original transfer;
3. verifies that it exists and is not itself a reversal;
4. verifies that the authenticated client owns the original destination account;
5. verifies that no reversal already exists;
6. locks both affected accounts in deterministic account-ID order;
7. verifies that both accounts exist and are active;
8. debits the original destination by the exact amount it received;
9. credits the original source by the exact original total debit;
10. inserts an immutable reversal transfer;
11. inserts two reversal account entries;
12. completes the idempotency record;
13. commits.

The original destination may become negative during reversal. A reversal is therefore allowed regardless of that account's current balance.

After a reversal creates a negative balance:

* ordinary outgoing transfers and FX transfers remain forbidden unless the balance covers the complete debit;
* incoming transfers remain allowed and may restore the balance.

A reversal uses the exact amounts, fee and rate snapshot stored in the original transfer. It never uses a current exchange rate or recalculates the original fee.

The partial unique index on `reverses_transfer_id` is the final database guarantee against concurrent duplicate reversals.

## 12. Idempotency

Account creation, transfer, FX transfer and reversal endpoints accept an `Idempotency-Key` header where applicable.

A normalized fingerprint includes the business-significant request fields. For a same-currency transfer this includes at least:

```text
source_account_id
destination_account_id
currency
amount
idempotency_key scope
```

For FX, the fingerprint is based on client intent and does not include the server-selected exchange rate or calculated fee.

Behaviour:

* a new key executes the operation;
* the same key with the same fingerprint returns the original HTTP status and response body;
* the same key with a different fingerprint returns `409 idempotency_conflict`;
* concurrent requests with the same key produce one authoritative outcome;
* successful results are stored;
* deterministic business errors are stored for the idempotency window;
* transient infrastructure failures and unexpected internal errors are not stored as final outcomes.

Examples of stored deterministic errors include insufficient funds, account not found, account not active, currency mismatch, same-account transfer, unauthorized reversal and already-reversed transfer.

The default retention window is 24 hours and is configurable.

## 13. History queries

### Account history

Account history is read from `account_entries` using the selected account ID. Because the query is account-scoped, one business operation appears once from that account's perspective.

```sql
SELECT ...
FROM account_entries
WHERE account_id = $1
  AND (created_at, id) < ($2, $3)
ORDER BY created_at DESC, id DESC
LIMIT $4;
```

### Account-pair history

History between two accounts is also account-relative:

```sql
SELECT ...
FROM account_entries
WHERE account_id = $1
  AND counterparty_account_id = $2
  AND (created_at, id) < ($3, $4)
ORDER BY created_at DESC, id DESC
LIMIT $5;
```

This avoids doubled rows: the debit entry belongs to one account and the credit entry belongs to the other.

History responses contain material summary data only, such as direction, amount, currency, operation kind, counterparty and timestamp. Full FX rate and reversal details are returned by `GET /v1/transfers/{transfer_id}`.

### Cursor pagination

History uses keyset/cursor pagination rather than `OFFSET`.

* stable order: `created_at DESC, id DESC`;
* cursor contents: the last returned `(created_at, id)` pair;
* default limit: 50;
* maximum limit: 100.

Using both fields makes ordering deterministic when multiple entries share the same timestamp. Newer inserts do not shift or duplicate subsequent pages.

## 14. Consistency and concurrency

PostgreSQL is the only authoritative source for:

* account balances and statuses;
* completed transfers;
* account entries;
* reversals;
* exchange-rate snapshots used by completed FX transfers;
* idempotency state.

No cache participates in transfer validation or balance mutation.

Operations on independent accounts may execute concurrently. Operations contending on the same account are intentionally serialized using row-level locks.

All multi-account operations lock account rows in deterministic ascending account-ID order. This reduces deadlock risk for opposing operations such as `A → B` and `B → A`.

The transfer, both balance updates, both account entries and the idempotency outcome are committed atomically.

## 15. API outline

```text
POST /v1/accounts
GET  /v1/accounts/{account_id}/balance

POST /v1/transfers
POST /v1/fx-transfers
GET  /v1/transfers/{transfer_id}
POST /v1/transfers/{transfer_id}/reversal

GET  /v1/accounts/{account_id}/entries
GET  /v1/accounts/{account_id}/entries?counterparty_account_id=...

GET  /health
GET  /ready
GET  /metrics
```

A same-owner transfer uses the same transfer endpoint as any other transfer. The only invalid self-transfer is one where source and destination account IDs are identical.

The complete request, response and error schemas are defined in the OpenAPI 3.1 specification.

## 16. Error handling

API errors use a stable structured representation:

```json
{
  "error": {
    "code": "insufficient_funds",
    "message": "The source account has insufficient funds"
  }
}
```

Expected error categories include:

* invalid request;
* unauthorized;
* forbidden;
* account not found;
* account not active;
* transfer not found;
* insufficient funds;
* currency mismatch;
* same-account transfer;
* exchange rate unavailable or expired;
* idempotency conflict;
* transfer already reversed;
* reversal not permitted;
* internal error.

A missing account returns `404`. An existing but inactive account returns a domain conflict such as `409 account_not_active`.

Internal database details are logged but never returned to API clients.

## 17. Performance strategy

The initial performance strategy focuses on reducing work in the critical transaction path:

* stateless Tokio/Axum instances;
* SQLx connection pooling;
* short database transactions;
* deterministic row locking;
* minimal database round trips;
* primary-key balance reads;
* account-entry history projections;
* cursor-based history pagination;
* prepared statement reuse;
* targeted indexing;
* no remote calls inside financial transactions;
* no authoritative distributed cache.

History summary queries normally read `account_entries` without joining the full transfer table. Detailed transfer reads use the transfer ID only when requested.

Connection-pool size is configurable. PgBouncer may be introduced later if measured connection scaling requires it.

Performance changes should be benchmark-driven rather than based on speculative caching.

## 18. Observability

The service uses structured tracing with request correlation IDs.

Logs and traces include:

* endpoint;
* request ID;
* authenticated client ID where appropriate;
* operation result;
* latency;
* database error category.

Sensitive values, JWTs and full financial request bodies are not logged.

Metrics may include:

* request count;
* request latency;
* transfer success and rejection counts;
* transaction duration;
* database pool utilization;
* deadlock and retry counts.

## 19. Testing strategy

Unit tests cover deterministic validation, decimal-string parsing, currency-scale enforcement, fee calculation, half-up FX rounding and overflow rejection.

Integration tests against PostgreSQL cover:

* account creation with zero and positive initial balances;
* rejection of negative initial balance;
* no account entry on account creation;
* balance reads;
* same-currency transfers;
* same-owner transfers;
* insufficient funds;
* currency mismatch;
* same-account rejection;
* missing and inactive accounts;
* authorization;
* account history and account-pair history;
* stable cursor pagination;
* successful FX transfer;
* FX fee and rounding;
* successful reversal;
* reversal authorization by original destination owner;
* reversal overdraft;
* duplicate and reversal-of-reversal rejection;
* idempotent success replay;
* idempotent deterministic-error replay;
* idempotency-key conflict.

Concurrency tests verify:

* concurrent transfers cannot create an ordinary overdraft;
* total funds are conserved for same-currency operations;
* opposing transfers do not corrupt balances;
* concurrent reversal attempts create only one reversal;
* concurrent requests with one idempotency key execute once.

Load tests report throughput and p50, p95 and p99 latency for representative account distributions, including independent accounts and intentionally contended hot accounts.

## 20. Scalability and future evolution

The service scales horizontally by adding stateless application instances against one PostgreSQL database.

The initial design can evolve through:

* additional account states and lifecycle endpoints;
* read replicas for non-authoritative history queries;
* time-based partitioning;
* PgBouncer;
* transactional outbox for event publishing;
* asynchronous read projections;
* externally managed exchange rates;
* asymmetric JWT verification;
* fine-grained account permissions.

These mechanisms are not introduced until measurements or requirements justify their operational complexity.

## 21. Key invariants

The implementation must preserve the following invariants:

* account initial balance is non-negative;
* account currency and scale are immutable;
* source and destination account IDs differ;
* both participating accounts exist and are active;
* normal and FX source debits never exceed the available balance;
* a negative balance may arise only through reversal domain logic;
* one completed business operation has one transfer and exactly two account entries;
* one original transfer has at most one reversal;
* a reversal cannot itself be reversed;
* full and partial monetary amounts are positive where applicable;
* idempotency-key reuse with a different fingerprint never executes the new request;
* balances, transfer records, account entries and idempotency outcomes change atomically.

## 22. Key trade-offs

### PostgreSQL instead of an authoritative cache

This favors correctness and operational simplicity and avoids dual-write consistency problems while still providing fast indexed balance access.

### Row locking instead of optimistic retries

Row locking gives predictable correctness under contention. Optimistic concurrency may perform better under very low contention but can cause repeated retries for hot accounts.

### Current balance plus immutable audit records

Storing the current balance avoids recalculating it from complete history. Atomic updates keep the current-state projection and immutable audit records consistent.

### Account entries as a partial read projection

Account entries deliberately duplicate only the summary fields needed for fast history reads. Full transfer details remain authoritative in `transfers`, avoiding both expensive history joins and complete duplication of every transfer twice.

### Cursor pagination instead of offsets

Cursor pagination keeps deep history queries efficient and prevents new inserts from shifting later pages.

### Stateless application instances

Stateless instances simplify horizontal scaling and failure recovery. PostgreSQL remains the shared coordination point and eventual throughput boundary.

# Technical Assessment: High-Performance Ledger Service (Rust)

## Overview

Your task is to develop a simplified Ledger Service using Rust. This service handles financial transactions between accounts. You must ensure data consistency, handle high concurrency, and implement safety mechanisms.

**Important:** Use of LLM tools is permitted.
However, you are responsible for the final architecture, code quality, and the ability to explain every decision.

## Tech Stack Requirements

- _Rust_ v1.92+
- _Runtime_ `tokio` v1.49+
- _Web Framework_ `axum` v0.8+
- _Database_ `PostgreSQL` v18+
- _Database Wrapper_ `sqlx` v0.8+

## Functional Requirements

The service must expose a REST API to perform the following operations.
The API must be described using OpenAPI 3.1.0+.

### Account Management

The system must support the initialization of accounts with an initial balance.

- **Create Account**: The API must accept an initial balance and currency and return the new account ID.
- **Get Balance**: The API must return the current balance for any given account ID.

### Money Transfer

The system must reliably transfer funds between two _distinct_ accounts.

The transfer must be atomic. It either succeeds completely or fails without side effects.

The system must prevent overdrafts.
The system must prevent transfers between accounts with different currencies.

#### Bonus: Cross-Currency Transfer

The system must support transfers between accounts with different currencies.
The system must apply a conversion fee.

### Transfer Rollback

The system must support the reversal of a previously completed transfer.

- **Reverse Transfer**: The API must accept a transfer ID and initiate a reversal.
- **Flow**: The system must deduct `amount` from the original destination account and return it to the original source account.
- **Overdraft**: The system must allow overdraft for the original destination account.

### Transfer History

The system must provide an audit trail of transactions.

- **Account history**: The API must return a list of all transfers for a specific account ID.
- **List Transfers**: The API must return a list of all transfers between two specific account IDs.

## Non-Functional Requirements

### Error Handling

The system must handle errors gracefully.
The API must return structured error objects including a code and a message.

### Security

The REST API must require JWT authorization to perform any operation.

### Performance

- **Concurrency**: The system must handle high concurrency.
- **Scale**: The system must handle scenarios with a large number of accounts and transfers.
- **Latency**: The system must show the lowest possible latency under any load.

### Scalability

The system must be horizontally scalable against the single database.

### Bonus: Observability

The system must provide visibility into its internal state.

- **Tracing**: The system must generate structured logs and traces.
- **Metrics**: The system must expose metrics.

## Deliverables

The solution must contain the following artifacts.

- **Design Document** (`design.md`): A detailed explanation of design choices, trade-offs, and compromises.
- **Source Code**: The complete Rust implementation. Comprehensive documentation is preferred.
- **OpenAPI Specification**: The API definition file.
- **Dockerfile**: A file to containerize the service (Preferred).
- **Test Suite**: A comprehensive set of tests (Preferred).
- **Benchmarks**: A set of performance benchmarks (Preferred).
- **Agent Context and Instructions**: A file containing the agent context and instructions (If applicable).

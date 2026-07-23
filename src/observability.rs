use std::{sync::OnceLock, time::Instant};

use anyhow::Result;
use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use metrics::{counter, histogram};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tracing::{Instrument, Span, info_span};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt::writer::BoxMakeWriter, prelude::*};
use uuid::Uuid;

use crate::api_error::AppError;

const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");
const METRICS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const BUCKETS: &[f64] = &[
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

#[derive(Clone, Debug)]
pub struct RequestId(pub String);

#[derive(Clone, Copy)]
pub enum FinancialOperation {
    Transfer,
    FxTransfer,
    Reversal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalOutcome {
    Success,
    Rejected,
    InternalError,
}

impl TerminalOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Rejected => "rejected",
            Self::InternalError => "internal_error",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdempotencyOutcome {
    Owner,
    Replay,
    Conflict,
}

impl IdempotencyOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Replay => "replay",
            Self::Conflict => "conflict",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinancialReason {
    None,
    InternalError,
    SameSourceAndDestination,
    AccountUnavailable,
    InsufficientFunds,
    RateUnavailable,
    RateConfigurationAmbiguous,
    FeeRuleUnavailable,
    FeeRuleConfigurationAmbiguous,
    DestinationAmountTooSmall,
    IdempotencyConflict,
    ReversalOfReversal,
    TransferAlreadyReversed,
    ArithmeticOverflow,
    MalformedAmount,
    TooManyFractionalDigits,
    NonPositiveAmount,
    AmountOverflow,
    MalformedAccountId,
    MalformedTransferId,
    InvalidJson,
    NotFound,
    BadRequest,
    Unauthorized,
    Forbidden,
    Conflict,
    ServiceUnavailable,
    InvalidRequest,
}

impl FinancialReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::InternalError => "internal_error",
            Self::SameSourceAndDestination => "same_source_and_destination",
            Self::AccountUnavailable => "account_unavailable",
            Self::InsufficientFunds => "insufficient_funds",
            Self::RateUnavailable => "rate_unavailable",
            Self::RateConfigurationAmbiguous => "rate_configuration_ambiguous",
            Self::FeeRuleUnavailable => "fee_rule_unavailable",
            Self::FeeRuleConfigurationAmbiguous => "fee_rule_configuration_ambiguous",
            Self::DestinationAmountTooSmall => "destination_amount_too_small",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::ReversalOfReversal => "reversal_of_reversal",
            Self::TransferAlreadyReversed => "transfer_already_reversed",
            Self::ArithmeticOverflow => "arithmetic_overflow",
            Self::MalformedAmount => "malformed_amount",
            Self::TooManyFractionalDigits => "too_many_fractional_digits",
            Self::NonPositiveAmount => "non_positive_amount",
            Self::AmountOverflow => "amount_overflow",
            Self::MalformedAccountId => "malformed_account_id",
            Self::MalformedTransferId => "malformed_transfer_id",
            Self::InvalidJson => "invalid_json",
            Self::NotFound => "not_found",
            Self::BadRequest => "bad_request",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::Conflict => "conflict",
            Self::ServiceUnavailable => "service_unavailable",
            Self::InvalidRequest => "invalid_request",
        }
    }
}

impl FinancialOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Transfer => "transfer",
            Self::FxTransfer => "fx_transfer",
            Self::Reversal => "reversal",
        }
    }
}

pub struct TransactionObservation {
    operation: FinancialOperation,
    started: Instant,
    completed: bool,
}

impl TransactionObservation {
    pub fn new(operation: FinancialOperation) -> Self {
        Self::started(operation, Instant::now())
    }

    pub fn started(operation: FinancialOperation, started: Instant) -> Self {
        Self {
            operation,
            started,
            completed: false,
        }
    }

    pub fn idempotency(&self, outcome: IdempotencyOutcome) {
        counter!("ledger_idempotency_outcomes_total", "operation" => self.operation.as_str(), "outcome" => outcome.as_str()).increment(1);
        tracing::info!(
            operation = self.operation.as_str(),
            outcome = outcome.as_str(),
            "ledger idempotency outcome"
        );
    }

    pub fn complete(&mut self, outcome: TerminalOutcome, reason: FinancialReason) {
        if self.completed {
            return;
        }
        self.completed = true;
        let duration = self.started.elapsed().as_secs_f64();
        counter!("ledger_operations_total", "operation" => self.operation.as_str(), "outcome" => outcome.as_str(), "reason" => reason.as_str()).increment(1);
        histogram!("ledger_database_transaction_duration_seconds", "operation" => self.operation.as_str(), "outcome" => outcome.as_str()).record(duration);
        tracing::info!(
            operation = self.operation.as_str(),
            outcome = outcome.as_str(),
            reason = reason.as_str(),
            transaction_duration_seconds = duration,
            "ledger operation completed"
        );
    }

    pub fn reject(&mut self, error: &AppError) {
        let (outcome, reason) = error.financial_metric_outcome();
        self.complete(outcome, reason);
    }
}

impl Drop for TransactionObservation {
    fn drop(&mut self) {
        if !self.completed {
            self.complete(
                TerminalOutcome::InternalError,
                FinancialReason::InternalError,
            );
        }
    }
}

pub fn record_operation_without_transaction(
    operation: FinancialOperation,
    outcome: TerminalOutcome,
    reason: FinancialReason,
) {
    counter!("ledger_operations_total", "operation" => operation.as_str(), "outcome" => outcome.as_str(), "reason" => reason.as_str()).increment(1);
    tracing::info!(
        operation = operation.as_str(),
        outcome = outcome.as_str(),
        reason = reason.as_str(),
        "ledger operation completed"
    );
}

static METRICS: OnceLock<PrometheusHandle> = OnceLock::new();

pub fn init_logging(log_filter: &str) -> Result<WorkerGuard> {
    let (writer, guard) = tracing_appender::non_blocking(std::io::stdout());
    let subscriber = tracing_subscriber::registry()
        .with(EnvFilter::try_new(log_filter)?)
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_current_span(true)
                .with_span_list(true)
                .with_writer(BoxMakeWriter::new(writer)),
        );
    tracing::subscriber::set_global_default(subscriber)?;
    Ok(guard)
}

pub fn metrics_handle() -> &'static PrometheusHandle {
    METRICS.get_or_init(|| {
        PrometheusBuilder::new()
            .set_buckets(BUCKETS)
            .expect("valid HTTP metric histogram buckets")
            .install_recorder()
            .expect("metrics recorder must be initialized once")
    })
}

pub async fn metrics() -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, METRICS_CONTENT_TYPE)],
        metrics_handle().render(),
    )
        .into_response()
}

pub async fn observe_request(mut request: Request, next: Next) -> Response {
    let request_id = effective_request_id(&request);
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));
    let method = normalized_method(request.method());
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|matched| matched.as_str().to_owned())
        .unwrap_or_else(|| "unmatched".to_owned());
    let span = request_span(&request_id, method, &route);
    let started = Instant::now();
    let mut response = next.run(request).instrument(span.clone()).await;
    let elapsed = started.elapsed().as_secs_f64();
    let status_class = status_class(response.status());

    counter!("ledger_http_requests_total", "method" => method, "route" => route.clone(), "status_class" => status_class).increment(1);
    histogram!("ledger_http_request_duration_seconds", "method" => method, "route" => route)
        .record(elapsed);
    tracing::info!(parent: &span, status = %response.status(), latency_seconds = elapsed, "HTTP request completed");

    response.headers_mut().insert(
        REQUEST_ID_HEADER,
        HeaderValue::from_str(&request_id)
            .expect("generated or validated request ID is a header value"),
    );
    response
}

fn request_span(request_id: &str, method: &str, route: &str) -> Span {
    info_span!("http.request", request_id, method, route)
}

fn effective_request_id(request: &Request) -> String {
    let values = request.headers().get_all(&REQUEST_ID_HEADER);
    let mut values = values.iter();
    match (values.next(), values.next()) {
        (Some(value), None) => value
            .to_str()
            .ok()
            .filter(|value| valid_request_id(value))
            .map(str::to_owned)
            .unwrap_or_else(new_request_id),
        _ => new_request_id(),
    }
}

fn valid_request_id(value: &str) -> bool {
    (1..=128).contains(&value.len()) && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn new_request_id() -> String {
    Uuid::new_v4().to_string()
}

fn status_class(status: StatusCode) -> &'static str {
    match status.as_u16() / 100 {
        2 => "2xx",
        3 => "3xx",
        4 => "4xx",
        _ => "5xx",
    }
}

fn normalized_method(method: &Method) -> &'static str {
    match method.as_str() {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        "CONNECT" => "CONNECT",
        "TRACE" => "TRACE",
        _ => "OTHER",
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{self, Write},
        sync::{Arc, Mutex},
    };

    use axum::{Router, body::Body, http::Request, middleware, routing::get};
    use tower::ServiceExt;

    use super::*;

    #[derive(Clone)]
    struct TestWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for TestWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn completion_logs_are_json_and_exclude_sensitive_request_data() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_current_span(true)
            .with_span_list(true)
            .with_writer(move || TestWriter(writer.clone()))
            .finish();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let resource_id = Uuid::new_v4();
        let request_id = "safe-request-id";

        tracing::subscriber::with_default(subscriber, || {
            runtime.block_on(async {
                let app = Router::new()
                    .route(
                        "/accounts/{account_id}/balance",
                        get(|| async { StatusCode::OK }),
                    )
                    .layer(middleware::from_fn(observe_request));
                let response = app
                    .oneshot(
                        Request::get(format!("/accounts/{resource_id}/balance?token=discard"))
                            .header("authorization", "Bearer sensitive-secret")
                            .header("x-request-id", request_id)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
            });
        });

        let logs = String::from_utf8(output.lock().unwrap().clone()).unwrap();
        let event = logs
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|value| value["fields"]["message"] == "HTTP request completed")
            .expect("completion event is emitted");
        assert_eq!(event["span"]["request_id"], request_id);
        assert_eq!(event["span"]["method"], "GET");
        assert_eq!(event["span"]["route"], "/accounts/{account_id}/balance");
        assert!(!logs.contains("sensitive-secret"));
        assert!(!logs.contains("token=discard"));
        assert!(!logs.contains(&resource_id.to_string()));
    }

    #[test]
    fn financial_metrics_use_only_bounded_labels() {
        let secret = Uuid::new_v4().to_string();
        let mut observation = TransactionObservation::new(FinancialOperation::Transfer);
        observation.idempotency(IdempotencyOutcome::Owner);
        observation.complete(
            TerminalOutcome::Rejected,
            FinancialReason::InsufficientFunds,
        );

        let metrics = metrics_handle().render();
        assert!(metrics.contains("ledger_operations_total"));
        assert!(metrics.contains("operation=\"transfer\""));
        assert!(metrics.contains("outcome=\"rejected\""));
        assert!(metrics.contains("reason=\"insufficient_funds\""));
        assert!(metrics.contains("ledger_idempotency_outcomes_total"));
        assert!(metrics.contains("outcome=\"owner\""));
        assert!(metrics.contains("ledger_database_transaction_duration_seconds"));
        assert!(!metrics.contains(&secret));
    }
}

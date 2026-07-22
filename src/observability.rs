use std::{sync::OnceLock, time::Instant};

use anyhow::Result;
use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use metrics::{counter, histogram};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tracing::{Instrument, Span, info_span};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt::writer::BoxMakeWriter, prelude::*};
use uuid::Uuid;

const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");
const METRICS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";
const BUCKETS: &[f64] = &[
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

#[derive(Clone, Debug)]
pub struct RequestId(pub String);

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
    let method = request.method().to_string();
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|matched| matched.as_str().to_owned())
        .unwrap_or_else(|| "unmatched".to_owned());
    let span = request_span(&request_id, &method, &route);
    let started = Instant::now();
    let mut response = next.run(request).instrument(span.clone()).await;
    let elapsed = started.elapsed().as_secs_f64();
    let status_class = status_class(response.status());

    counter!("ledger_http_requests_total", "method" => method.clone(), "route" => route.clone(), "status_class" => status_class).increment(1);
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

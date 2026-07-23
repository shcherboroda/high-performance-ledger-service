use std::{collections::BTreeMap, time::Instant};

use reqwest::{Client, StatusCode};
use serde::Serialize;

#[derive(Debug, Default, Clone, Serialize)]
pub struct Classifications {
    pub http_statuses: BTreeMap<u16, u64>,
    pub expected_business_rejections: u64,
    pub transport_failures: u64,
    pub timeout_failures: u64,
    pub parsing_failures: u64,
    pub unexpected_http_failures: u64,
}

#[derive(Debug, Clone)]
pub struct Operation {
    pub latency_ns: Option<u128>,
    pub classification: Classifications,
    pub valid: bool,
    pub transfer_id: Option<uuid::Uuid>,
}

#[derive(Clone)]
pub struct Http {
    client: Client,
    urls: Vec<String>,
}

impl Http {
    pub fn new(urls: Vec<String>, timeout: std::time::Duration) -> anyhow::Result<Self> {
        Ok(Self {
            client: Client::builder().timeout(timeout).build()?,
            urls,
        })
    }
    pub fn url_for(&self, operation: usize) -> &str {
        &self.urls[operation % self.urls.len()]
    }
    pub async fn create_account(
        &self,
        operation: usize,
        token: &str,
        idempotency_key: &str,
        initial_balance: &str,
    ) -> anyhow::Result<uuid::Uuid> {
        let response = self
            .client
            .post(format!(
                "{}/accounts",
                self.url_for(operation).trim_end_matches('/')
            ))
            .bearer_auth(token)
            .header("Idempotency-Key", idempotency_key)
            .json(&serde_json::json!({"currency":"USD","initial_balance":initial_balance}))
            .send()
            .await?;
        if response.status() != StatusCode::CREATED {
            anyhow::bail!("account setup returned HTTP {}", response.status());
        }
        let body: serde_json::Value = response.json().await?;
        body.get("id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("account setup response has no account ID"))
            .and_then(|id| uuid::Uuid::parse_str(id).map_err(Into::into))
    }
    pub async fn transfer(
        &self,
        operation: usize,
        token: &str,
        source: uuid::Uuid,
        destination: uuid::Uuid,
        key: &str,
    ) -> Operation {
        let started = Instant::now();
        let response = self.client.post(format!("{}/transfers", self.url_for(operation).trim_end_matches('/'))).bearer_auth(token).header("Idempotency-Key", key).json(&serde_json::json!({"source_account_id":source,"destination_account_id":destination,"amount":"1.00"})).send().await;
        let mut classification = Classifications::default();
        match response {
            Ok(response) => {
                let status = response.status();
                *classification
                    .http_statuses
                    .entry(status.as_u16())
                    .or_default() += 1;
                if status == StatusCode::CREATED {
                    match response.json::<serde_json::Value>().await {
                        Ok(body)
                            if body.get("id").and_then(serde_json::Value::as_str).is_some() =>
                        {
                            Operation {
                                latency_ns: Some(started.elapsed().as_nanos()),
                                classification,
                                valid: true,
                                transfer_id: body
                                    .get("id")
                                    .and_then(serde_json::Value::as_str)
                                    .and_then(|id| uuid::Uuid::parse_str(id).ok()),
                            }
                        }
                        _ => {
                            classification.parsing_failures += 1;
                            Operation {
                                latency_ns: Some(started.elapsed().as_nanos()),
                                classification,
                                valid: false,
                                transfer_id: None,
                            }
                        }
                    }
                } else {
                    classification.unexpected_http_failures += 1;
                    Operation {
                        latency_ns: Some(started.elapsed().as_nanos()),
                        classification,
                        valid: false,
                        transfer_id: None,
                    }
                }
            }
            Err(error) => {
                if error.is_timeout() {
                    classification.timeout_failures += 1;
                } else {
                    classification.transport_failures += 1;
                }
                Operation {
                    latency_ns: Some(started.elapsed().as_nanos()),
                    classification,
                    valid: false,
                    transfer_id: None,
                }
            }
        }
    }
    pub async fn metrics(&self, operation: usize) -> Result<String, String> {
        self.client
            .get(format!(
                "{}/metrics",
                self.url_for(operation).trim_end_matches('/')
            ))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .error_for_status()
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())
    }
}

pub fn merge(into: &mut Classifications, next: &Classifications) {
    for (status, count) in &next.http_statuses {
        *into.http_statuses.entry(*status).or_default() += count;
    }
    into.expected_business_rejections += next.expected_business_rejections;
    into.transport_failures += next.transport_failures;
    into.timeout_failures += next.timeout_failures;
    into.parsing_failures += next.parsing_failures;
    into.unexpected_http_failures += next.unexpected_http_failures;
}

pub struct PhaseOperations {
    pub warmup: Vec<Operation>,
    pub measured: Vec<Operation>,
}

pub fn summarize_measured(phases: &PhaseOperations) -> (Classifications, Vec<u128>, bool) {
    let mut classifications = Classifications::default();
    let mut samples = Vec::with_capacity(phases.measured.len());
    let mut valid = true;
    for operation in &phases.measured {
        if let Some(latency) = operation.latency_ns {
            samples.push(latency);
        }
        merge(&mut classifications, &operation.classification);
        valid &= operation.valid;
    }
    (classifications, samples, valid)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn url_distribution_is_deterministic() {
        let client = Http::new(
            vec!["http://a".into(), "http://b".into()],
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(client.url_for(0), "http://a");
        assert_eq!(client.url_for(3), "http://b");
    }

    #[test]
    fn only_measured_operations_are_aggregated() {
        let warmup = Operation {
            latency_ns: Some(1),
            classification: Classifications {
                unexpected_http_failures: 1,
                ..Default::default()
            },
            valid: false,
            transfer_id: None,
        };
        let measured = Operation {
            latency_ns: Some(100),
            classification: Classifications {
                http_statuses: BTreeMap::from([(201, 1)]),
                ..Default::default()
            },
            valid: true,
            transfer_id: None,
        };
        let (classifications, samples, valid) = summarize_measured(&PhaseOperations {
            warmup: vec![warmup],
            measured: vec![measured],
        });
        assert_eq!(samples, vec![100]);
        assert_eq!(classifications.http_statuses.get(&201), Some(&1));
        assert_eq!(classifications.unexpected_http_failures, 0);
        assert!(valid);
    }
}

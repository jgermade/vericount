//! Sending billing records to the AEAT and keeping track of the results.
//!
//! Records are sent in chain order. A record that the AEAT rejected stops
//! the queue: it has to be corrected before later records are sent.

use std::collections::HashMap;

use serde::Serialize;
use sqlx::PgPool;
use tokio::sync::Mutex;
use vericounts_core::aeat::{
    self, Operation, Party, Record, RecordStatus, ResponseError, SoftwareSystem, SubmissionStatus,
    MAX_RECORDS_PER_REQUEST,
};

use crate::{
    aeat_client::{AeatClient, SubmitError},
    config::Issuer,
    store::{self, StoreError},
};

/// State of a record with respect to the AEAT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordState {
    /// Not sent yet, or sent without a usable answer.
    Pending,
    Accepted,
    /// Registered by the AEAT, but with errors to correct.
    AcceptedWithErrors,
    Rejected,
}

impl RecordState {
    fn from_db(status: &str) -> Self {
        match status {
            "accepted" => Self::Accepted,
            "accepted_with_errors" => Self::AcceptedWithErrors,
            "rejected" => Self::Rejected,
            _ => Self::Pending,
        }
    }

    fn registered(self) -> bool {
        matches!(self, Self::Accepted | Self::AcceptedWithErrors)
    }

    /// The state that matters most when several records are summarized.
    pub fn worst(states: impl IntoIterator<Item = Self>) -> Self {
        let rank = |state: &Self| match state {
            Self::Rejected => 3,
            Self::Pending => 2,
            Self::AcceptedWithErrors => 1,
            Self::Accepted => 0,
        };
        states.into_iter().max_by_key(rank).unwrap_or(Self::Pending)
    }
}

/// Latest result of a record.
#[derive(Debug, Clone, Serialize)]
pub struct RecordResult {
    pub state: RecordState,
    pub error_code: Option<String>,
    pub error_description: Option<String>,
}

/// Latest result of every record that has one.
pub async fn record_results(
    pool: &PgPool,
    issuer_nif: &str,
) -> Result<HashMap<i64, RecordResult>, StoreError> {
    let rows: Vec<(i64, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT DISTINCT ON (r.record_id) r.record_id, r.status, r.error_code, r.error_description
         FROM aeat_record_results r JOIN aeat_submissions s ON s.id = r.submission_id
         WHERE s.issuer_nif = $1
         ORDER BY r.record_id, r.submission_id DESC",
    )
    .bind(issuer_nif)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(record_id, status, error_code, error_description)| {
            (
                record_id,
                RecordResult {
                    state: RecordState::from_db(&status),
                    error_code,
                    error_description,
                },
            )
        })
        .collect())
}

/// Everything needed to send records.
pub struct Submitter {
    pub client: AeatClient,
    pub system: SoftwareSystem,
    /// Only one submission runs at a time in this process.
    lock: Mutex<()>,
}

impl Submitter {
    pub fn new(client: AeatClient, system: SoftwareSystem) -> Self {
        Self {
            client,
            system,
            lock: Mutex::new(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum RunOutcome {
    /// Every record is registered at the AEAT.
    NothingPending,
    /// The next record to send was rejected and must be corrected first.
    Blocked { record_id: i64 },
    Sent {
        submission_id: i64,
        records: usize,
        outcome: String,
        wait_seconds: Option<u32>,
    },
}

struct Outcome {
    outcome: &'static str,
    http_status: Option<i32>,
    response_xml: Option<String>,
    csv: Option<String>,
    wait_seconds: Option<u32>,
    error: Option<String>,
    results: Vec<(i64, &'static str, Option<String>, Option<String>)>,
}

impl Outcome {
    fn failure(outcome: &'static str, error: String) -> Self {
        Self {
            outcome,
            http_status: None,
            response_xml: None,
            csv: None,
            wait_seconds: None,
            error: Some(error),
            results: Vec::new(),
        }
    }
}

fn key(record: &Record) -> (&str, Operation) {
    match record {
        Record::Registration(record) => (&record.series_number, Operation::Registration),
        Record::Cancellation(record) => (&record.series_number, Operation::Cancellation),
    }
}

/// Sends the next batch of pending records, if any, and stores what
/// happened.
pub async fn run_once(
    pool: &PgPool,
    issuer: &Issuer,
    submitter: &Submitter,
) -> Result<RunOutcome, StoreError> {
    let _guard = submitter.lock.lock().await;

    let records = store::chain_records(pool, &issuer.nif).await?;
    let results = record_results(pool, &issuer.nif).await?;
    let state = |id: &i64| results.get(id).map_or(RecordState::Pending, |r| r.state);

    let Some(first) = records.iter().position(|(id, _)| !state(id).registered()) else {
        return Ok(RunOutcome::NothingPending);
    };
    if state(&records[first].0) == RecordState::Rejected {
        return Ok(RunOutcome::Blocked {
            record_id: records[first].0,
        });
    }
    let batch: Vec<&(i64, Record)> = records[first..]
        .iter()
        .take_while(|(id, _)| state(id) == RecordState::Pending)
        .take(MAX_RECORDS_PER_REQUEST)
        .collect();
    let batch_records: Vec<Record> = batch.iter().map(|(_, record)| record.clone()).collect();
    let request = aeat::submission_request(
        &Party {
            name: issuer.name.clone(),
            nif: issuer.nif.clone(),
        },
        &submitter.system,
        &batch_records,
    );

    // The submission is stored before sending: if the process dies in
    // between, the attempt is still on record.
    let mut tx = pool.begin().await?;
    let submission_id: i64 = sqlx::query_scalar(
        "INSERT INTO aeat_submissions (issuer_nif, request_xml) VALUES ($1, $2) RETURNING id",
    )
    .bind(&issuer.nif)
    .bind(aeat::soap_envelope(&request))
    .fetch_one(&mut *tx)
    .await?;
    for (record_id, _) in &batch {
        sqlx::query(
            "INSERT INTO aeat_submission_records (submission_id, record_id) VALUES ($1, $2)",
        )
        .bind(submission_id)
        .bind(record_id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    let outcome = match submitter.client.submit(&request).await {
        Ok(submission) => {
            let response = submission.response;
            let mut results = Vec::new();
            for line in &response.records {
                let Some((record_id, _)) = batch.iter().find(|(_, record)| {
                    key(record) == (line.series_number.as_str(), line.operation)
                }) else {
                    continue;
                };
                let status = match line.status {
                    RecordStatus::Accepted => "accepted",
                    RecordStatus::AcceptedWithErrors => "accepted_with_errors",
                    RecordStatus::Rejected => "rejected",
                };
                results.push((
                    *record_id,
                    status,
                    line.error_code.clone(),
                    line.error_description.clone(),
                ));
            }
            Outcome {
                outcome: match response.status {
                    SubmissionStatus::Accepted => "accepted",
                    SubmissionStatus::PartiallyAccepted => "partially_accepted",
                    SubmissionStatus::Rejected => "rejected",
                },
                http_status: Some(200),
                response_xml: Some(submission.response_xml),
                csv: response.csv,
                wait_seconds: Some(response.wait_seconds),
                error: None,
                results,
            }
        }
        Err(SubmitError::Http { status, body }) => Outcome {
            http_status: Some(i32::from(status)),
            response_xml: Some(body),
            ..Outcome::failure("http_error", format!("HTTP {status}"))
        },
        Err(SubmitError::Response { error, body }) => Outcome {
            response_xml: Some(body),
            ..Outcome::failure(
                match error {
                    ResponseError::Fault { .. } => "fault",
                    ResponseError::Malformed(_) => "malformed_response",
                },
                error.to_string(),
            )
        },
        Err(error @ (SubmitError::Transport(_) | SubmitError::Setup(_))) => {
            Outcome::failure("transport_error", error.to_string())
        }
    };

    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO aeat_responses
             (submission_id, outcome, http_status, response_xml, csv, wait_seconds, error)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(submission_id)
    .bind(outcome.outcome)
    .bind(outcome.http_status)
    .bind(&outcome.response_xml)
    .bind(&outcome.csv)
    .bind(outcome.wait_seconds.map(|seconds| seconds as i32))
    .bind(&outcome.error)
    .execute(&mut *tx)
    .await?;
    for (record_id, status, error_code, error_description) in &outcome.results {
        sqlx::query(
            "INSERT INTO aeat_record_results
                 (submission_id, record_id, status, error_code, error_description)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(submission_id)
        .bind(record_id)
        .bind(status)
        .bind(error_code)
        .bind(error_description)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    Ok(RunOutcome::Sent {
        submission_id,
        records: batch.len(),
        outcome: outcome.outcome.to_owned(),
        wait_seconds: outcome.wait_seconds,
    })
}

/// Summary of the submission queue, for the user interface.
#[derive(Debug, Serialize)]
pub struct QueueStatus {
    pub enabled: bool,
    pub pending: usize,
    pub rejected: usize,
    pub accepted_with_errors: usize,
    /// Outcome and error of the most recent attempt.
    pub last_outcome: Option<String>,
    pub last_error: Option<String>,
}

pub async fn queue_status(
    pool: &PgPool,
    issuer_nif: &str,
    enabled: bool,
) -> Result<QueueStatus, StoreError> {
    let results = record_results(pool, issuer_nif).await?;
    let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM billing_records WHERE issuer_nif = $1")
        .bind(issuer_nif)
        .fetch_all(pool)
        .await?;
    let count = |wanted: RecordState| {
        ids.iter()
            .filter(|id| results.get(id).map_or(RecordState::Pending, |r| r.state) == wanted)
            .count()
    };
    let last: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT r.outcome, r.error FROM aeat_responses r
         JOIN aeat_submissions s ON s.id = r.submission_id
         WHERE s.issuer_nif = $1 ORDER BY r.submission_id DESC LIMIT 1",
    )
    .bind(issuer_nif)
    .fetch_optional(pool)
    .await?;
    Ok(QueueStatus {
        enabled,
        pending: count(RecordState::Pending),
        rejected: count(RecordState::Rejected),
        accepted_with_errors: count(RecordState::AcceptedWithErrors),
        last_outcome: last.as_ref().map(|(outcome, _)| outcome.clone()),
        last_error: last.and_then(|(_, error)| error),
    })
}

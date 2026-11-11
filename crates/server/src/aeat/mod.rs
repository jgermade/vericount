//! Sending the billing records to the AEAT.
//!
//! - `client`: the SOAP client of the AEAT service.
//! - `submission`: the queue: which records are sent, in what order, and
//!   what came back.
//! - `service`: the practice or live submitter, the practice mock inside
//!   the server, and the uploaded certificate, stored encrypted.
//! - `certificate`: reading whose a certificate is and when it expires.
//!
//! Tables: `aeat_submissions`, `aeat_submission_records`, `aeat_responses`,
//! `aeat_record_results`, `aeat_certificate`.

pub mod api;
pub mod certificate;
pub mod client;
pub mod service;
pub mod submission;

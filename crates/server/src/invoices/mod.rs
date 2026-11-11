//! Invoices and their Veri*factu billing records: issuing, cancelling,
//! correcting a rejected record, the hash chain and its verification, the
//! quarter's totals, and the practice and live modes they are kept in.
//!
//! Everything here is insert-only. Tables: `invoices`, `invoice_lines`,
//! `invoice_taxes`, `billing_records`, `live_mode`.

pub mod store;

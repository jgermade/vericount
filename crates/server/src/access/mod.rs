//! Access: who may use vericount and how they prove it.
//!
//! - `users`: invitations, the list of users, removing access.
//! - `sessions`: sign-in codes by email, sessions and their cookie, and the
//!   middleware that checks them (CSRF included).
//! - `passkeys`: registering passkeys and signing in with them.
//!
//! Tables: `users`, `sessions`, `login_codes`, `passkeys`,
//! `webauthn_ceremonies`.

pub mod api;
pub mod passkeys;
pub mod sessions;
pub mod users;

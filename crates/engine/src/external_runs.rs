//! External runs: a read-only poller that observes the codespaces behind a
//! Space's recorded runs with the user's own `gh` login. See
//! `docs/internal/plans/2026-11-21-external-runs.md`.

mod cache;
mod gh;
mod poller;
mod service;
pub(crate) mod launcher;

#[cfg(not(test))]
pub use gh::SystemGh;
pub use service::ExternalRuns;

#[cfg(test)]
mod poller_tests;

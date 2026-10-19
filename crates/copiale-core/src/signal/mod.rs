//! Signal through presage: the [`Linker`] and [`SignalBackend`] the app uses.
//!
//! Talks to Signal's production servers; tests use the testkit fakes, and
//! only the mapping to core types is tested here.
//!
//! [`Linker`]: crate::backend::Linker
//! [`SignalBackend`]: crate::backend::SignalBackend

mod backend;
mod errors;
mod incoming;
mod protocol;
mod store;

pub use backend::{PresageBackend, PresageLinker};

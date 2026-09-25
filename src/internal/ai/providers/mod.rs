//! Completion providers retained for repository Memory compilation.
//!
//! The general Code provider factory was removed with the Code executor. The
//! DSH Memory bridge only needs DeepSeek, while deterministic Memory tests use
//! the fixture-backed fake provider.

pub mod deepseek;
#[cfg(test)]
pub mod fake;
pub(crate) mod openai_compat;
pub(crate) mod wire_helpers;

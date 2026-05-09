//! Per-workspace approximate in-flight counts on MQ Redis (design in
//! `docs/plans/`).

pub mod constants;
mod layer;
pub mod redis_ops;
mod service;

pub use layer::WorkspaceConcurrencyLayer;

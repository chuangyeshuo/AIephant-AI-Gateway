//! Global per-client-IP sliding 1s rate limit (`global.client-ip-rate-limit`).

mod layer;
mod memory;
mod redis_script;
pub mod resolve_ip;

pub use layer::ClientIpRateLimitLayer;

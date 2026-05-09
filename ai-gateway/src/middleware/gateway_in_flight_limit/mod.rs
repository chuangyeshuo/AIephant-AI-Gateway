//! Whole-gateway in-flight HTTP cap (`global.gateway-in-flight-limit`).

mod layer;
mod memory;
mod redis_release;
mod redis_script;

pub use layer::GatewayInFlightLimitLayer;

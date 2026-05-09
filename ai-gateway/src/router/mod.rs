pub mod direct;
pub mod latency;
pub mod meta;
pub mod router_details;
pub mod service;
pub mod strategy;
pub mod target_provider_resolve;
pub mod unified_api;

pub(in crate::router) const FORCED_ROUTING_HEADER: http::HeaderName =
    http::HeaderName::from_static("alephant-forced-routing");

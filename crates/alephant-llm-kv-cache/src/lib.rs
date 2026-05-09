#![forbid(unsafe_code)]

#[cfg(all(feature = "internal", feature = "external"))]
compile_error!("enable only one of: internal, external");

#[cfg(not(any(feature = "internal", feature = "external")))]
compile_error!("enable exactly one of: internal, external");

mod backend;
mod backoff;
pub mod bucket;
pub mod error;
#[cfg(feature = "internal")]
mod internal_stub;
pub mod key;
#[cfg(feature = "external")]
mod put_retry;
pub mod response;
pub mod settings;
#[cfg(feature = "internal")]
mod tikv_util;
pub mod value;

#[cfg(feature = "external")]
pub mod cloudflare;
#[cfg(feature = "external")]
mod lazy_cloudflare;

pub use backend::LlmKvBackend;
pub use bucket::{read_bucket, try_save_to_first_free_slot};
#[cfg(feature = "internal")]
pub use internal_stub::InternalStubBackend;
#[cfg(feature = "internal")]
mod lazy_tikv;
#[cfg(feature = "internal")]
mod tikv;
pub use key::kv_key_sha256_hex;
#[cfg(feature = "external")]
pub use lazy_cloudflare::LazyCloudflareKvBackend;
#[cfg(feature = "internal")]
pub use lazy_tikv::LazyTikvBackend;
pub use response::{apply_alephant_cache_hit_headers, merge_cached_headers};
pub use settings::CacheSettings;
#[cfg(feature = "internal")]
pub use tikv::TikvKvClient;
pub use value::LlmCacheEntry;

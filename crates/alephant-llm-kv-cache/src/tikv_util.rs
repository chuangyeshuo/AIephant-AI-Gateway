//! Helpers for TiKV TTL normalization and value size checks (no TiKV
//! connection).

/// Normalizes TTL for storage: `0` maps to **60** seconds (aligned with
/// Cloudflare KV default in this codebase).
pub fn normalized_ttl_secs(expiration_ttl_secs: u64) -> u64 {
    if expiration_ttl_secs == 0 {
        60
    } else {
        expiration_ttl_secs
    }
}

/// Returns `true` when `len` is strictly greater than `max` (Cloudflare-style
/// skip on oversize).
pub fn value_exceeds_limit(len: usize, max: usize) -> bool {
    len > max
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_ttl_zero_becomes_60() {
        assert_eq!(normalized_ttl_secs(0), 60);
        assert_eq!(normalized_ttl_secs(3600), 3600);
    }

    #[test]
    fn value_exceeds_limit_matches_cloudflare_style() {
        let max = 8 * 1024 * 1024;
        assert!(!value_exceeds_limit(max, max));
        assert!(value_exceeds_limit(max + 1, max));
    }
}

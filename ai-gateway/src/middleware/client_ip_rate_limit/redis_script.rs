//! Redis sliding-window script (server clock).

use redis::Script;

/// Lua single-key sliding 1s window; returns 1 allow, 0 reject.
pub fn sliding_window_script() -> Script {
    Script::new(include_str!("client_ip_sliding.lua"))
}

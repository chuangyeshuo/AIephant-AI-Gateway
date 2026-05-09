//! For **gate JUnit / lib-test-captures** archival: serialize inputs and
//! outputs as one JSON line on stdout.
//!
//! Rust's stock `#[test]` does not record function arguments or return values;
//! unless the test prints explicitly, JUnit only shows libtest boilerplate. For
//! structured I/O, call `gate_test_io!` around assertions.

/// Serialize input and output as JSON, `println` on one line for nextest to
/// capture into JUnit `system-out`.
#[macro_export]
macro_rules! gate_test_io {
    ($input:expr, $output:expr) => {{
        let row = serde_json::json!({
            "gate_test_io": {
                "input": serde_json::to_value(&$input).unwrap_or(serde_json::Value::Null),
                "output": serde_json::to_value(&$output).unwrap_or(serde_json::Value::Null),
            }
        });
        println!("{}", row);
    }};
}

#[cfg(test)]
mod tests {
    #[test]
    fn gate_test_io_emits_single_json_line() {
        gate_test_io!(serde_json::json!({ "k": 1 }), "expected");
    }
}

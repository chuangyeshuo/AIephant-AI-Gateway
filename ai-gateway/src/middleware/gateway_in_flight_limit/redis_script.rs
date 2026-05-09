use redis::Script;

#[must_use]
pub fn acquire_in_flight_script() -> Script {
    Script::new(include_str!("acquire_in_flight.lua"))
}

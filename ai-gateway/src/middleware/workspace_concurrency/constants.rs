use uuid::Uuid;

pub const KEY_PREFIX: &str = "lc:mq:rmt:concurrency:ws:";
pub const TTL_SECS: i64 = 3600;

#[must_use]
pub fn redis_key(workspace: Uuid) -> String {
    format!("{KEY_PREFIX}{workspace}")
}

#[must_use]
pub fn placeholder_workspace() -> Uuid {
    Uuid::nil()
}

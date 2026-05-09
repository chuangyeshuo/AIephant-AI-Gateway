//! `enrichment_touch` NOTIFY scope values (Postgres `pg_notify` ↔ gateway).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentTouchScope {
    VirtualKey,
    Workspace,
    Agent,
    Member,
    Department,
}

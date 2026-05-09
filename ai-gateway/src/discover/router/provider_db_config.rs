//! Converts raw DB rows from `providers` / `provider_models` into the
//! in-memory structures used by the gateway:
//!
//! * [`ProvidersConfig`] — global provider registry (base URL + models).
//! * `HashMap<RouterId, RouterConfig>` — one derived route per enabled
//!   provider, using a single-provider `BalancedLatency` strategy.
//!
//! The conversion starts from the **embedded** `ProvidersConfig::default()` so
//! that providers without a `default_base_url` in the DB still get a sensible
//! base URL from the bundled YAML.  DB data then overlays:
//! 1. models come from `provider_models` (converted with provider context);
//! 2. `default_base_url` (when non-null) overrides the embedded default;
//! 3. providers absent from DB or disabled are removed.

use std::collections::HashMap;

use indexmap::IndexSet;
use nonempty_collections::nes;
use tracing::{debug, warn};
use uuid::Uuid;

use super::bare_model_expand_index::BareModelExpandIndex;
use crate::{
    config::{
        balance::{BalanceConfig, BalanceConfigInner},
        providers::{GlobalProviderConfig, ProvidersConfig},
        router::RouterConfig,
    },
    endpoints::EndpointType,
    store::router::{DbGatewayProvider, DbGatewayProviderModel},
    types::{model_id::ModelId, provider::InferenceProvider, router::RouterId},
};

/// Build [`ProvidersConfig`] and a derived router map from raw DB rows.
///
/// Returns:
/// * `ProvidersConfig` — updated view of all enabled providers.
/// * `HashMap<RouterId, RouterConfig>` — one route per enabled provider. Each
///   route uses `BalancedLatency` with a single-provider `NESet` so the
///   existing load-balancer infrastructure handles it uniformly.
/// * [`BareModelExpandIndex`] — bare `model_id` → `code/model` (aligned with DB
///   rows successfully ingested into `GlobalProviderConfig.models`).
#[allow(clippy::too_many_lines)]
pub fn build_from_db(
    db_providers: &[DbGatewayProvider],
    db_models: &[DbGatewayProviderModel],
) -> (
    ProvidersConfig,
    HashMap<RouterId, RouterConfig>,
    BareModelExpandIndex,
) {
    // Index raw model name strings by provider_id for O(1) lookup.
    let mut raw_models_by_provider: HashMap<Uuid, Vec<String>> = HashMap::new();
    for row in db_models {
        raw_models_by_provider
            .entry(row.provider_id)
            .or_default()
            .push(row.model_id.clone());
    }

    // Start from the embedded defaults so every known provider has a base URL.
    let embedded_defaults = ProvidersConfig::default();

    let mut entries: Vec<(InferenceProvider, GlobalProviderConfig)> = Vec::new();
    let mut router_map: HashMap<RouterId, RouterConfig> = HashMap::new();
    let mut bare_model_expand = BareModelExpandIndex::default();

    for db_provider in db_providers {
        let Ok(provider) = InferenceProvider::from_provider_code(&db_provider.code) else {
            warn!(
                code = %db_provider.code,
                "provider_db_config: unknown provider code, skipping"
            );
            continue;
        };

        // Base URL: DB override takes precedence; fall back to embedded YAML.
        let base_url = if let Some(url_str) = &db_provider.default_base_url {
            match url_str.parse::<url::Url>() {
                Ok(u) => u,
                Err(e) => {
                    warn!(
                        code = %db_provider.code,
                        url = %url_str,
                        error = %e,
                        "provider_db_config: invalid base_url, falling back to embedded default"
                    );
                    if let Some(c) = embedded_defaults.get(&provider) {
                        c.base_url.clone()
                    } else {
                        warn!(
                            code = %db_provider.code,
                            "provider_db_config: no base_url and no embedded default, skipping provider"
                        );
                        continue;
                    }
                }
            }
        } else if let Some(c) = embedded_defaults.get(&provider) {
            c.base_url.clone()
        } else {
            if !matches!(provider, InferenceProvider::Custom) {
                warn!(
                    code = %db_provider.code,
                    "provider_db_config: no base_url in DB and no embedded default, skipping provider"
                );
            }
            continue;
        };

        // Convert raw model name strings to typed ModelId using provider
        // context.
        let raw_models = raw_models_by_provider
            .remove(&db_provider.id)
            .unwrap_or_default();
        let mut models = IndexSet::new();
        for model_str in &raw_models {
            match ModelId::from_str_and_provider(provider.clone(), model_str) {
                Ok(m) => {
                    models.insert(m);
                    bare_model_expand.push(&db_provider.code, model_str);
                }
                Err(e) => {
                    warn!(
                        code = %db_provider.code,
                        model = %model_str,
                        error = %e,
                        "provider_db_config: failed to parse model_id, skipping"
                    );
                }
            }
        }

        // Preserve version header from embedded config (e.g. Anthropic).
        let version = embedded_defaults
            .get(&provider)
            .and_then(|c| c.version.clone());

        // Preserve upstream auth style from embedded config (e.g. Xiaomi
        // `api-key`).
        let upstream_auth = embedded_defaults
            .get(&provider)
            .map(|c| c.upstream_auth)
            .unwrap_or_default();

        debug!(
            code = %db_provider.code,
            models = models.len(),
            "provider_db_config: registered provider"
        );

        entries.push((
            provider.clone(),
            GlobalProviderConfig {
                models,
                base_url,
                version,
                upstream_auth,
            },
        ));

        // Derived single-provider router (BalancedLatency with one provider).
        let router_id =
            RouterId::Named(compact_str::CompactString::from(db_provider.code.as_str()));
        let load_balance = BalanceConfig::from(HashMap::from([(
            EndpointType::Chat,
            BalanceConfigInner::BalancedLatency {
                providers: nes![provider],
            },
        )]));
        router_map.insert(
            router_id,
            RouterConfig {
                load_balance,
                ..RouterConfig::default()
            },
        );
    }

    let providers_config: ProvidersConfig = entries.into_iter().collect();
    (providers_config, router_map, bare_model_expand)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use compact_str::CompactString;
    use indexmap::IndexSet;
    use uuid::Uuid;

    use super::{super::BareModelExpandIndex, *};

    #[test]
    fn build_from_db_uses_db_base_url_and_builds_single_provider_router() {
        let provider_id = Uuid::new_v4();
        let db_providers = vec![DbGatewayProvider {
            id: provider_id,
            code: "openai".to_string(),
            default_base_url: Some("https://override.openai.test".to_string()),
            updated_at: Utc::now(),
            is_router: false,
        }];
        let db_models = vec![
            DbGatewayProviderModel {
                provider_id,
                model_id: "gpt-4o".to_string(),
            },
            DbGatewayProviderModel {
                provider_id,
                model_id: String::new(), // invalid model id should be skipped
            },
        ];

        let (providers_config, router_map, bare_expand) = build_from_db(&db_providers, &db_models);
        let bare_gpt = bare_expand.gateway_models_for_bare_id("gpt-4o");
        assert_eq!(bare_gpt, vec!["openai/gpt-4o".to_string()]);

        let openai_cfg = providers_config
            .get(&InferenceProvider::OpenAI)
            .expect("openai config should exist");
        assert_eq!(
            openai_cfg.base_url.as_str(),
            "https://override.openai.test/"
        );

        let expected_models: IndexSet<ModelId> =
            IndexSet::from_iter([ModelId::from_str_and_provider(
                InferenceProvider::OpenAI,
                "gpt-4o",
            )
            .expect("valid model")]);
        assert_eq!(openai_cfg.models, expected_models);

        let router_id = RouterId::Named(CompactString::new("openai"));
        let router_cfg = router_map
            .get(&router_id)
            .expect("derived openai router should exist");
        let providers = router_cfg.load_balance.providers();
        assert_eq!(providers.len(), 1);
        assert!(providers.contains(&InferenceProvider::OpenAI));
    }

    #[test]
    fn build_from_db_skips_unknown_provider_codes() {
        let db_providers = vec![DbGatewayProvider {
            id: Uuid::new_v4(),
            code: "unknown-provider-code".to_string(),
            default_base_url: Some("https://unknown.test".to_string()),
            updated_at: Utc::now(),
            is_router: false,
        }];

        let (providers_config, router_map, bare_expand) = build_from_db(&db_providers, &[]);
        assert!(providers_config.is_empty());
        assert!(router_map.is_empty());
        assert_eq!(bare_expand, BareModelExpandIndex::default());
    }

    #[test]
    fn build_from_db_registers_z_ai_code() {
        let provider_id = Uuid::new_v4();
        let db_providers = vec![DbGatewayProvider {
            id: provider_id,
            code: "z-ai".to_string(),
            default_base_url: Some("https://api.z.ai/api/paas/v4/".to_string()),
            updated_at: Utc::now(),
            is_router: false,
        }];
        let db_models = vec![DbGatewayProviderModel {
            provider_id,
            model_id: "glm-5".to_string(),
        }];

        let (providers_config, router_map, _bare_expand) = build_from_db(&db_providers, &db_models);

        let z_ai = InferenceProvider::Named("z-ai".into());
        let cfg = providers_config.get(&z_ai).expect("z-ai providers config");
        assert_eq!(cfg.base_url.as_str(), "https://api.z.ai/api/paas/v4/");

        assert!(router_map.contains_key(&RouterId::Named(compact_str::CompactString::new("z-ai"))));
    }

    #[test]
    fn build_from_db_falls_back_to_embedded_base_url_when_db_url_missing() {
        let provider_id = Uuid::new_v4();
        let db_providers = vec![DbGatewayProvider {
            id: provider_id,
            code: "anthropic".to_string(),
            default_base_url: None,
            updated_at: Utc::now(),
            is_router: false,
        }];

        let (providers_config, router_map, _bare_expand) = build_from_db(&db_providers, &[]);

        let embedded_defaults = ProvidersConfig::default();
        let expected_base_url = embedded_defaults
            .get(&InferenceProvider::Anthropic)
            .expect("embedded anthropic exists")
            .base_url
            .clone();
        let anthropic_cfg = providers_config
            .get(&InferenceProvider::Anthropic)
            .expect("anthropic config should exist");
        assert_eq!(anthropic_cfg.base_url, expected_base_url);

        let router_id = RouterId::Named(CompactString::new("anthropic"));
        assert!(router_map.contains_key(&router_id));
    }

    /// When the same `model_id` exists under two `providers.code` values, the
    /// expand index lists two `code/model` entries (`code` must parse via
    /// [`InferenceProvider::from_provider_code`]).
    #[test]
    fn build_from_db_bare_index_lists_all_providers_for_same_model_id() {
        let prov_groq = Uuid::new_v4();
        let prov_deepseek = Uuid::new_v4();
        let db_providers = vec![
            DbGatewayProvider {
                id: prov_groq,
                code: "groq".to_string(),
                default_base_url: Some("https://groq.test".to_string()),
                updated_at: Utc::now(),
                is_router: false,
            },
            DbGatewayProvider {
                id: prov_deepseek,
                code: "deepseek".to_string(),
                default_base_url: Some("https://deepseek.test".to_string()),
                updated_at: Utc::now(),
                is_router: false,
            },
        ];
        let shared = "gpt-4o";
        let db_models = vec![
            DbGatewayProviderModel {
                provider_id: prov_groq,
                model_id: shared.to_string(),
            },
            DbGatewayProviderModel {
                provider_id: prov_deepseek,
                model_id: shared.to_string(),
            },
        ];

        let (_cfg, _map, bare) = build_from_db(&db_providers, &db_models);
        let v = bare.gateway_models_for_bare_id(shared);
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v.contains(&"groq/gpt-4o".to_string()));
        assert!(v.contains(&"deepseek/gpt-4o".to_string()));
    }
}

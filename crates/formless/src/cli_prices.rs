use crate::table::{c, t, Table, Tone};
use anyhow::Result;
use formless_core::config::{ModelPrice, ProviderCfg, DEFAULT_PROVIDER};
use formless_core::{Config, Paths};
use std::collections::{BTreeMap, BTreeSet};

struct ModelReference<'a> {
    matched_key: Option<&'a str>,
    price: Option<&'a ModelPrice>,
}

struct ProviderPrices<'a> {
    prices: BTreeMap<&'a str, &'a ModelPrice>,
    configured_models: BTreeMap<&'a str, ModelReference<'a>>,
}

pub(crate) fn list(paths: &Paths, name: Option<&str>, json: bool) -> Result<()> {
    let cfg = Config::load_file_only(paths)?;
    let mut providers: BTreeMap<&str, &ProviderCfg> = cfg
        .providers
        .iter()
        .map(|(name, provider)| (name.as_str(), provider))
        .collect();
    providers.insert(DEFAULT_PROVIDER, &cfg.provider);
    if let Some(name) = name {
        if !providers.contains_key(name) {
            return Err(crate::usage(format!("unknown provider '{name}'")));
        }
        providers.retain(|provider, _| *provider == name);
    }

    let reports: BTreeMap<_, _> = providers
        .into_iter()
        .map(|(name, provider)| {
            let role_models = cfg
                .roles
                .values()
                .filter(|role| {
                    role.provider
                        .as_deref()
                        .filter(|provider| !provider.is_empty())
                        .unwrap_or(DEFAULT_PROVIDER)
                        == name
                })
                .map(|role| &role.model);
            let models: BTreeSet<_> = std::iter::once(&provider.model)
                .chain(&provider.fallback_models)
                .chain(role_models)
                .map(String::as_str)
                .filter(|model| !model.trim().is_empty() && !ProviderCfg::is_auto_model(model))
                .collect();
            let configured_models = models
                .into_iter()
                .map(|model| {
                    let entry = provider.price_entry_for(model);
                    (
                        model,
                        ModelReference {
                            matched_key: entry.map(|(key, _)| key),
                            price: entry.map(|(_, price)| price),
                        },
                    )
                })
                .collect();
            (
                name,
                ProviderPrices {
                    prices: provider
                        .prices
                        .iter()
                        .map(|(key, price)| (key.as_str(), price))
                        .collect(),
                    configured_models,
                },
            )
        })
        .collect();

    if json {
        use serde_json::{json, Map};
        let providers: Map<_, _> = reports
            .into_iter()
            .map(|(name, report)| {
                let configured_models: Map<_, _> = report
                    .configured_models
                    .into_iter()
                    .map(|(model, reference)| {
                        (
                            model.to_owned(),
                            json!({
                                "matched_key": reference.matched_key,
                                "price": reference.price,
                            }),
                        )
                    })
                    .collect();
                (
                    name.to_owned(),
                    json!({
                        "prices": report.prices,
                        "configured_models": configured_models,
                    }),
                )
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "source": "config_file_and_defaults",
                "unit": "usd_per_million_tokens",
                "providers": providers,
            }))?
        );
        return Ok(());
    }

    println!("Source is the configuration file and defaults. Environment overrides are excluded.");
    println!("Stored prices (USD per million tokens)");
    let mut prices = Table::new(&[
        "PROVIDER",
        "MODEL",
        "IN",
        "OUT",
        "CACHED IN",
        "CACHE WRITE",
        "RETRIEVED",
    ]);
    let mut references = Table::new(&["PROVIDER", "CONFIGURED MODEL", "STORED PRICE KEY"]);
    let rate = |value: Option<f64>| value.map_or_else(|| "-".to_owned(), |v| v.to_string());
    for (name, report) in reports {
        for (model, price) in report.prices {
            prices.row(vec![
                c(name),
                t(model, Tone::Strong),
                c(price.input.to_string()),
                c(price.output.to_string()),
                c(rate(price.cached_input)),
                c(rate(price.cache_write_input)),
                c(price.retrieved_at.as_deref().unwrap_or("-")),
            ]);
        }
        for (model, reference) in report.configured_models {
            references.row(vec![
                c(name),
                t(model, Tone::Strong),
                reference
                    .matched_key
                    .map_or_else(|| t("no price", Tone::Warn), c),
            ]);
        }
    }
    if prices.is_empty() {
        println!("No stored price entries.");
    } else {
        prices.print();
    }
    println!();
    if references.is_empty() {
        println!("No configured model references.");
    } else {
        references.print();
    }
    Ok(())
}

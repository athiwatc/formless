//! The model picker (SPEC §17): one overlay for the setup wizard and
//! `/model`, filtered by fuzzy (subsequence) matching on the model id.

/// One pickable model: a provider model id, or a Role (`role:<name>`).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelItem {
    pub id: String,
    /// Where it comes from, shown when there is more than one source.
    pub source: Option<String>,
    pub recent: bool,
    /// What it is and can do: "→ claude-sonnet" for an alias; context,
    /// price per million tokens, vision, tools, and a failed test.
    pub detail: Option<String>,
}

/// "200k · $3/$15 · vision · tools · ✗ test failed" from a model's
/// `ListModels` metadata and remembered test (SPEC §4.2, §4.3).
pub fn model_detail(meta: &serde_json::Value, test: &serde_json::Value) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(c) = meta["context"].as_u64() {
        parts.push(if c >= 1_000_000 {
            format!("{}M ctx", c / 1_000_000)
        } else {
            format!("{}k ctx", c / 1000)
        });
    }
    if let (Some(i), Some(o)) = (meta["input_price"].as_f64(), meta["output_price"].as_f64()) {
        let fmt = |v: f64| if v.fract() == 0.0 { format!("{v:.0}") } else { format!("{v:.2}") };
        parts.push(format!("${}/${} per M", fmt(i), fmt(o)));
    }
    if meta["vision"] == true {
        parts.push("vision".into());
    }
    match meta["tools"].as_bool() {
        Some(true) => parts.push("tools".into()),
        Some(false) => parts.push("no tools".into()),
        None => {}
    }
    if test.is_object() {
        parts.push(if test["ok"] == true {
            "✓ tested".into()
        } else {
            "✗ test failed".into()
        });
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

pub use formless_core::fuzzy::fuzzy_match;

/// Picker rows from a `ListModels` result: Roles, then the provider's
/// models, then recent models it didn't list, each id once.
pub fn items_from_list(result: &serde_json::Value) -> Vec<ModelItem> {
    let strs = |v: &serde_json::Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m.as_str().map(str::to_string))
            .collect()
    };
    let recent = strs(&result["recent"]);
    let mut items: Vec<ModelItem> = Vec::new();
    let mut push = |id: String, source: String| {
        if !items.iter().any(|i| i.id == id) {
            let detail = model_detail(&result["meta"][&id], &result["tests"][&id]);
            items.push(ModelItem {
                recent: recent.contains(&id),
                id,
                source: Some(source),
                detail,
            });
        }
    };
    // Aliases first: short names for the models people use most.
    for (alias, target) in result["aliases"].as_object().into_iter().flatten() {
        let target = target.as_str().unwrap_or_default();
        push(alias.clone(), format!("alias → {target}"));
    }
    for r in result["roles"].as_array().into_iter().flatten() {
        let name = r["name"].as_str().unwrap_or_default();
        let provider = r["provider"].as_str().unwrap_or("default");
        let model = r["model"]
            .as_str()
            .filter(|m| !m.is_empty())
            .unwrap_or("its default model");
        push(
            format!("role:{name}"),
            format!("role · {provider} · {model}"),
        );
    }
    for m in strs(&result["models"]) {
        push(m, "default provider".into());
    }
    for m in recent.clone() {
        push(m, "recent".into());
    }
    items
}

pub struct ModelPicker {
    pub query: String,
    pub items: Vec<ModelItem>,
    /// Index into `matches()`.
    pub sel: usize,
    pub loading: bool,
    /// Why the list is empty (e.g. `GET /models` failed); typing a model
    /// id still works.
    pub error: Option<String>,
}

impl ModelPicker {
    pub fn new(items: Vec<ModelItem>) -> Self {
        Self {
            query: String::new(),
            items,
            sel: 0,
            loading: false,
            error: None,
        }
    }

    pub fn loading() -> Self {
        Self {
            loading: true,
            ..Self::new(Vec::new())
        }
    }

    /// Fill the list, recent models first, the rest in the given order.
    pub fn set_items(&mut self, mut items: Vec<ModelItem>, select: &str) {
        items.sort_by_key(|i| !i.recent);
        self.sel = items.iter().position(|i| i.id == select).unwrap_or(0);
        self.items = items;
        self.loading = false;
    }

    /// Matching items, best first (ties keep list order, so recent first),
    /// with the matched char positions.
    pub fn matches(&self) -> Vec<(&ModelItem, Vec<usize>)> {
        let mut out: Vec<(i64, usize, &ModelItem, Vec<usize>)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(n, it)| fuzzy_match(&self.query, &it.id).map(|(s, p)| (s, n, it, p)))
            .collect();
        if !self.query.trim().is_empty() {
            out.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        out.into_iter().map(|(_, _, it, p)| (it, p)).collect()
    }

    /// The chosen model: the highlighted match, or the typed text as a
    /// free-form id when nothing matches.
    pub fn choice(&self) -> Option<String> {
        let matches = self.matches();
        match matches.get(self.sel.min(matches.len().saturating_sub(1))) {
            Some((it, _)) => Some(it.id.clone()),
            None => Some(self.query.trim().to_string()).filter(|q| !q.is_empty()),
        }
    }

    pub fn type_char(&mut self, c: char) {
        self.query.push(c);
        self.sel = 0;
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.sel = 0;
    }

    pub fn up(&mut self) {
        self.sel = self.sel.saturating_sub(1);
    }

    pub fn down(&mut self) {
        let n = self.matches().len();
        self.sel = (self.sel + 1).min(n.saturating_sub(1));
    }

    /// Whether rows should name their source.
    pub fn multi_source(&self) -> bool {
        let mut it = self.items.iter().map(|i| i.source.as_deref());
        let first = it.next();
        it.any(|s| Some(s) != first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(ids: &[&str]) -> Vec<ModelItem> {
        ids.iter()
            .map(|id| ModelItem {
                id: id.to_string(),
                source: None,
                recent: false,
                detail: None,
            })
            .collect()
    }

    #[test]
    fn son4_finds_sonnet_among_hundreds() {
        let mut ids: Vec<String> = (0..300).map(|i| format!("vendor{i}/model-{i}b")).collect();
        ids.push("anthropic/claude-3-haiku".into());
        ids.push("anthropic/claude-sonnet-4".into());
        ids.push("anthropic/claude-opus-4".into());
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let mut p = ModelPicker::new(items(&refs));
        for c in "son4".chars() {
            p.type_char(c);
        }
        assert_eq!(p.choice().as_deref(), Some("anthropic/claude-sonnet-4"));
    }

    #[test]
    fn ranks_word_starts_and_runs_higher() {
        let mut p = ModelPicker::new(items(&["gemini-pro", "gpt-4o-mini", "o4-mini", "llama"]));
        for c in "mini".chars() {
            p.type_char(c);
        }
        let got: Vec<&str> = p.matches().iter().map(|(i, _)| i.id.as_str()).collect();
        assert_eq!(got, vec!["o4-mini", "gpt-4o-mini", "gemini-pro"]);
    }

    #[test]
    fn recent_first_and_free_form_fallback() {
        let mut p = ModelPicker::loading();
        let mut list = items(&["a-model", "b-model", "c-model"]);
        list[2].recent = true;
        p.set_items(list, "b-model");
        assert_eq!(p.items[0].id, "c-model");
        assert_eq!(p.choice().as_deref(), Some("b-model"));
        for c in "my-own-model".chars() {
            p.type_char(c);
        }
        assert!(p.matches().is_empty());
        assert_eq!(p.choice().as_deref(), Some("my-own-model"));
    }

    #[test]
    fn items_from_a_list_models_result() {
        let result = serde_json::json!({
            "models": ["m1", "m2"],
            "roles": [{"name": "fast", "provider": "local", "model": ""}],
            "recent": ["m2", "custom"],
        });
        let got = items_from_list(&result);
        let ids: Vec<&str> = got.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["role:fast", "m1", "m2", "custom"]);
        assert_eq!(
            got[0].source.as_deref(),
            Some("role · local · its default model")
        );
        assert!(got[2].recent && got[3].recent && !got[1].recent);
    }

    #[test]
    fn selection_stays_in_range() {
        let mut p = ModelPicker::new(items(&["x1", "x2"]));
        p.down();
        p.down();
        p.down();
        assert_eq!(p.choice().as_deref(), Some("x2"));
        p.up();
        p.up();
        assert_eq!(p.choice().as_deref(), Some("x1"));
    }
}

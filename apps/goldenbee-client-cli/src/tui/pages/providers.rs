//! Providers management page state.

use common::ProviderMetadata;

/// Providers page state.
#[derive(Default)]
pub struct ProvidersPage {
    /// Provider catalog.
    pub providers: Vec<ProviderMetadata>,
    /// Highlighted row in the filtered view.
    pub selected: usize,
    /// Case-insensitive filter text.
    pub filter: String,
    /// Whether typed characters edit the filter.
    pub filtering: bool,
    /// Whether the catalog has loaded at least once.
    pub loaded: bool,
}

impl ProvidersPage {
    /// Replace the catalog and keep the selection valid.
    pub fn set(&mut self, providers: Vec<ProviderMetadata>) {
        self.providers = providers;
        self.loaded = true;
        self.clamp();
    }

    /// Replace one provider's metadata, adding it when absent.
    pub fn upsert(&mut self, provider: ProviderMetadata) {
        match self
            .providers
            .iter_mut()
            .find(|candidate| candidate.id == provider.id)
        {
            Some(existing) => *existing = provider,
            None => self.providers.push(provider),
        }
        self.providers.sort_by(|left, right| left.id.cmp(&right.id));
        self.clamp();
    }

    /// Filtered catalog.
    pub fn filtered(&self) -> Vec<&ProviderMetadata> {
        if self.filter.is_empty() {
            return self.providers.iter().collect();
        }
        let needle = self.filter.to_lowercase();
        self.providers
            .iter()
            .filter(|provider| {
                provider.id.to_lowercase().contains(&needle)
                    || provider.adapter.to_lowercase().contains(&needle)
                    || provider.model.to_lowercase().contains(&needle)
            })
            .collect()
    }

    /// Highlighted provider.
    pub fn selected_provider(&self) -> Option<&ProviderMetadata> {
        self.filtered().get(self.selected).copied()
    }

    /// Look up one provider by id.
    pub fn provider(&self, id: &str) -> Option<&ProviderMetadata> {
        self.providers.iter().find(|provider| provider.id == id)
    }

    /// Move the highlight up.
    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the highlight down.
    pub fn down(&mut self) {
        if self.selected + 1 < self.filtered().len() {
            self.selected += 1;
        }
    }

    /// Start editing the filter.
    pub fn start_filter(&mut self) {
        self.filtering = true;
        self.filter.clear();
        self.selected = 0;
    }

    /// Stop editing the filter.
    pub fn stop_filter(&mut self) {
        self.filtering = false;
    }

    /// Add one filter character.
    pub fn filter_push(&mut self, character: char) {
        self.filter.push(character);
        self.selected = 0;
    }

    /// Remove one filter character.
    pub fn filter_pop(&mut self) {
        self.filter.pop();
        self.selected = 0;
    }

    /// Table rows in the display order required by the page.
    pub fn rows(&self) -> Vec<Vec<String>> {
        self.filtered()
            .iter()
            .map(|provider| {
                vec![
                    provider.id.clone(),
                    provider.adapter.clone(),
                    empty(&provider.model),
                    provider.ready.to_string(),
                    provider.active.to_string(),
                    provider.revision.to_string(),
                    credential(provider),
                    workers(provider),
                    provider.models.len().to_string(),
                ]
            })
            .collect()
    }

    fn clamp(&mut self) {
        let length = self.filtered().len();
        if length == 0 {
            self.selected = 0;
        } else if self.selected >= length {
            self.selected = length - 1;
        }
    }
}

/// Column headers rendered by the Providers page.
pub const HEADERS: [&str; 9] = [
    "ID",
    "ADAPTER",
    "MODEL",
    "READY",
    "ACTIVE",
    "REV",
    "CREDENTIAL",
    "WORKERS",
    "MODELS",
];

fn empty(value: &str) -> String {
    if value.is_empty() {
        "-".to_string()
    } else {
        value.to_string()
    }
}

fn credential(provider: &ProviderMetadata) -> String {
    if provider.credential_revoked {
        return "revoked".to_string();
    }
    match provider.credential_version {
        Some(version) => format!("v{version}"),
        None => "-".to_string(),
    }
}

fn workers(provider: &ProviderMetadata) -> String {
    if provider.allowed_workers.is_empty() {
        "-".to_string()
    } else {
        provider.allowed_workers.join(",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str, model: &str) -> ProviderMetadata {
        ProviderMetadata {
            id: id.into(),
            adapter: "deterministic".into(),
            model: model.into(),
            endpoint: None,
            modalities: vec!["text".into()],
            tool_calling: false,
            context_window: None,
            local: true,
            residency: "local".into(),
            allowed_workers: vec!["worker-a".into()],
            models: vec![model.into()],
            models_source: "explicit".into(),
            discovered_at: None,
            allowed_models: None,
            allow_auto_default: false,
            ready: true,
            discovery_error: None,
            discovery_error_code: None,
            revision: 1,
            active: true,
            credential_version: Some(1),
            credential_fingerprint: None,
            credential_revoked: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn provider_columns_match_the_required_shape() {
        let mut page = ProvidersPage::default();
        page.set(vec![provider("local", "deterministic")]);
        let rows = page.rows();
        assert_eq!(rows[0].len(), HEADERS.len());
        assert_eq!(rows[0][0], "local");
        assert_eq!(rows[0][6], "v1");
        assert_eq!(rows[0][8], "1");
    }

    #[test]
    fn upsert_replaces_and_sorts_catalog_entries() {
        let mut page = ProvidersPage::default();
        page.set(vec![provider("z", "one")]);
        page.upsert(provider("a", "two"));
        assert_eq!(page.providers[0].id, "a");
        page.upsert(provider("a", "three"));
        assert_eq!(page.provider("a").unwrap().model, "three");
    }
}

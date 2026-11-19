//! Provider-owned model management page state.

use common::ProviderMetadata;

/// One discovered model and its provider-owned flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRow {
    /// Owning provider id.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Whether the provider allow-list includes this model.
    pub allowed: bool,
    /// Whether this is the preferred provider model.
    pub preferred: bool,
}

/// Models page state.
#[derive(Default)]
pub struct ModelsPage {
    /// Restrict the page to one provider, or show every provider.
    pub provider: Option<String>,
    /// Highlighted row.
    pub selected: usize,
    /// Case-insensitive filter text.
    pub filter: String,
    /// Whether typed characters edit the filter.
    pub filtering: bool,
}

impl ModelsPage {
    /// Build rows from the loaded provider catalog.
    pub fn rows(&self, providers: &[ProviderMetadata]) -> Vec<ModelRow> {
        let needle = self.filter.to_lowercase();
        providers
            .iter()
            .filter(|provider| {
                self.provider
                    .as_ref()
                    .is_none_or(|selected| selected == &provider.id)
            })
            .flat_map(|provider| {
                let allowed = provider.allowed_models.as_ref();
                provider.models.iter().map(move |model| ModelRow {
                    provider: provider.id.clone(),
                    model: model.clone(),
                    allowed: allowed.is_none_or(|allowed| allowed.contains(model)),
                    preferred: provider.model == *model,
                })
            })
            .filter(|row| {
                needle.is_empty()
                    || row.model.to_lowercase().contains(&needle)
                    || row.provider.to_lowercase().contains(&needle)
            })
            .collect()
    }

    /// Highlighted model row.
    pub fn selected_row(&self, providers: &[ProviderMetadata]) -> Option<ModelRow> {
        self.rows(providers).get(self.selected).cloned()
    }

    /// Move the highlight up.
    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move the highlight down.
    pub fn down(&mut self, length: usize) {
        if self.selected + 1 < length {
            self.selected += 1;
        }
    }

    /// Keep the selection valid after a refresh.
    pub fn clamp(&mut self, providers: &[ProviderMetadata]) {
        let length = self.rows(providers).len();
        if length == 0 {
            self.selected = 0;
        } else if self.selected >= length {
            self.selected = length - 1;
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

    /// Table rows in display order.
    pub fn table_rows(&self, providers: &[ProviderMetadata]) -> Vec<Vec<String>> {
        self.rows(providers)
            .into_iter()
            .map(|row| {
                vec![
                    row.provider,
                    row.model,
                    row.allowed.to_string(),
                    row.preferred.to_string(),
                ]
            })
            .collect()
    }
}

/// Column headers rendered by the Models page.
pub const HEADERS: [&str; 4] = ["PROVIDER", "MODEL", "ALLOWED", "PREFERRED"];

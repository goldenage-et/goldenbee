//! Workers management page state.

use common::AdminWorkerSummary;

/// Workers page state.
#[derive(Default)]
pub struct WorkersPage {
    /// Full admin inventory, newest load last.
    pub workers: Vec<AdminWorkerSummary>,
    /// Highlighted row in the filtered view.
    pub selected: usize,
    /// Case-insensitive filter text.
    pub filter: String,
    /// Whether typed characters edit the filter.
    pub filtering: bool,
    /// Whether the inventory has loaded at least once.
    pub loaded: bool,
}

impl WorkersPage {
    /// Replace the inventory and keep the selection valid.
    pub fn set(&mut self, workers: Vec<AdminWorkerSummary>) {
        self.workers = workers;
        self.loaded = true;
        self.clamp();
    }

    /// Filtered inventory.
    pub fn filtered(&self) -> Vec<&AdminWorkerSummary> {
        if self.filter.is_empty() {
            return self.workers.iter().collect();
        }
        let needle = self.filter.to_lowercase();
        self.workers
            .iter()
            .filter(|worker| {
                worker.id.to_lowercase().contains(&needle)
                    || worker.endpoint.to_lowercase().contains(&needle)
                    || worker.instance.to_lowercase().contains(&needle)
                    || worker.package.to_lowercase().contains(&needle)
                    || worker.status.to_lowercase().contains(&needle)
            })
            .collect()
    }

    /// Highlighted Worker.
    pub fn selected_worker(&self) -> Option<&AdminWorkerSummary> {
        self.filtered().get(self.selected).copied()
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
            .map(|worker| {
                vec![
                    worker.id.clone(),
                    worker.status.clone(),
                    worker.reachable.to_string(),
                    worker.revision.to_string(),
                    worker.source.clone(),
                    worker.instance.clone(),
                    worker.package.clone(),
                    worker.endpoint.clone(),
                    capabilities(worker),
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

/// Column headers rendered by the Workers page.
pub const HEADERS: [&str; 9] = [
    "ID",
    "STATUS",
    "REACH",
    "REV",
    "SOURCE",
    "INSTANCE",
    "PACKAGE",
    "ENDPOINT",
    "CAPABILITIES",
];

fn capabilities(worker: &AdminWorkerSummary) -> String {
    if let Some(capabilities) = &worker.capabilities {
        if !capabilities.is_empty() {
            return capabilities.join(",");
        }
    }
    worker
        .approved_capabilities
        .iter()
        .map(|capability| capability.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(id: &str, package: &str) -> AdminWorkerSummary {
        AdminWorkerSummary {
            id: id.into(),
            endpoint: format!("http://{id}"),
            instance: id.into(),
            package: package.into(),
            capabilities: Some(vec!["general".into()]),
            approved_capabilities: Vec::new(),
            declared_capabilities: Vec::new(),
            enabled: true,
            revision: 1,
            source: "config".into(),
            status: "approved".into(),
            reachable: true,
            control_token_set: true,
            incarnation: "1".into(),
            incarnations: Vec::new(),
            locality: "local".into(),
            lease_expires_at: 0,
            renewed_at: 0,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn filter_keeps_matching_rows_and_resets_selection() {
        let mut page = WorkersPage::default();
        page.set(vec![
            worker("worker-a", "echo"),
            worker("worker-b", "coding"),
        ]);
        page.selected = 1;
        page.start_filter();
        page.filter_push('e');
        page.filter_push('c');
        assert_eq!(page.filtered().len(), 1);
        assert_eq!(page.selected_worker().unwrap().id, "worker-a");
        page.filter_pop();
        page.filter_pop();
        assert_eq!(page.filtered().len(), 2);
    }

    #[test]
    fn columns_match_the_required_inventory_shape() {
        let mut page = WorkersPage::default();
        page.set(vec![worker("worker-a", "echo")]);
        let rows = page.rows();
        assert_eq!(rows[0].len(), HEADERS.len());
        assert_eq!(rows[0][0], "worker-a");
        assert_eq!(rows[0][8], "general");
    }
}

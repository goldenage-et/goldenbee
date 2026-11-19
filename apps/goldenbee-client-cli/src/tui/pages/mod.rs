//! Management pages.

pub mod chat;
pub mod models;
pub mod providers;
pub mod workers;

/// Selectable management pages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    /// Approved Workers.
    Workers,
    /// Model providers.
    Providers,
    /// Provider-owned models.
    Models,
}

impl Page {
    /// Next page in tab order.
    pub fn next(self) -> Self {
        match self {
            Page::Workers => Page::Providers,
            Page::Providers => Page::Models,
            Page::Models => Page::Workers,
        }
    }

    /// Previous page in tab order.
    pub fn previous(self) -> Self {
        match self {
            Page::Workers => Page::Models,
            Page::Providers => Page::Workers,
            Page::Models => Page::Providers,
        }
    }

    /// Index used by the tab bar.
    pub fn index(self) -> usize {
        match self {
            Page::Workers => 0,
            Page::Providers => 1,
            Page::Models => 2,
        }
    }
}

/// Tab labels in display order.
pub const LABELS: [&str; 3] = ["Workers", "Providers", "Models"];

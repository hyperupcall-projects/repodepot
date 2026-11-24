//! Messages for the app's log: what happened, when, and how much it matters.
//!
//! The backend sends entries with [`crate::backend::Event::Log`]; the UI also turns other events
//! (a sync finishing, a scan failing) into entries. The status bar shows the latest one.

use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    Warning,
    Error,
}

impl Level {
    pub const ALL: [Level; 3] = [Level::Info, Level::Warning, Level::Error];

    pub fn icon(self) -> &'static str {
        match self {
            Level::Info => "ℹ",
            Level::Warning => "⚠",
            Level::Error => "✖",
        }
    }

    /// For lists of entries at this level.
    pub fn plural(self) -> &'static str {
        match self {
            Level::Info => "Info",
            Level::Warning => "Warnings",
            Level::Error => "Errors",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub level: Level,
    pub at: SystemTime,
    /// One line, shown in the status bar.
    pub message: String,
    /// More lines, shown under the message in the log.
    pub detail: Option<String>,
}

impl Entry {
    pub fn new(level: Level, message: impl Into<String>) -> Self {
        Self {
            level,
            at: SystemTime::now(),
            message: message.into(),
            detail: None,
        }
    }

    pub fn info(message: impl Into<String>) -> Self {
        Self::new(Level::Info, message)
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(Level::Warning, message)
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(Level::Error, message)
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        self.detail = (!detail.trim().is_empty()).then_some(detail);
        self
    }

    /// Also writes the entry to stderr through `tracing`.
    pub fn trace(&self) {
        let detail = self.detail.as_deref().unwrap_or("").replace('\n', "; ");
        let sep = if detail.is_empty() { "" } else { " — " };
        match self.level {
            Level::Info => tracing::info!("{}{sep}{detail}", self.message),
            Level::Warning => tracing::warn!("{}{sep}{detail}", self.message),
            Level::Error => tracing::error!("{}{sep}{detail}", self.message),
        }
    }
}

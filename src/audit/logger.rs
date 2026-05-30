//! Append-only JSONL audit log.
//!
//! Every cycle produces one [`AuditEntry`] written as a single JSON line. The
//! file is opened in append mode and each write is flushed so the trail
//! survives crashes.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::error::{Result, TraderError};
use crate::llm::{OrderAttempt, ToolCallRecord};

/// One audit record per trading cycle.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    pub cycle_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub strategy_name: String,
    pub mode: String,
    pub dry_run: bool,
    pub final_response: String,
    pub iterations: u32,
    pub tool_calls: Vec<ToolCallRecord>,
    pub orders_attempted: Vec<OrderAttempt>,
}

/// Serialises audit entries to a JSONL file.
pub struct AuditLogger {
    file: Mutex<tokio::fs::File>,
}

impl AuditLogger {
    /// Open (creating if needed) the audit log under `dir/file`.
    pub async fn open(dir: &str, file: &str) -> Result<Self> {
        tokio::fs::create_dir_all(dir).await?;
        let path = Path::new(dir).join(file);
        let handle = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await?;
        Ok(Self {
            file: Mutex::new(handle),
        })
    }

    /// Append one entry as a JSON line.
    pub async fn log(&self, entry: &AuditEntry) -> Result<()> {
        let mut line =
            serde_json::to_string(entry).map_err(|e| TraderError::Serialize(e.to_string()))?;
        line.push('\n');
        let mut guard = self.file.lock().await;
        guard.write_all(line.as_bytes()).await?;
        guard.flush().await?;
        Ok(())
    }
}

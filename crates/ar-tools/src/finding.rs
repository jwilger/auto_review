use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Note,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Finding {
    pub source_tool: String,
    pub rule_id: Option<String>,
    pub path: String,
    pub line_start: u32,
    pub line_end: u32,
    pub severity: Severity,
    pub message: String,
}

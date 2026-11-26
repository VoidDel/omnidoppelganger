//! Backward-compatible structured IPC error codes and envelope helpers (`P2-13` & `P3-1`).
//!
//! Formats IPC errors as `[ERR_<CODE>] <message>` (or `[ERR_<CODE>] k=v;k2=v2 | <message>` when
//! structured parameters are attached), seamlessly matching `src/lib/errorCode.ts` (`parseBackendError`)
//! and `odg_core::skills::ERR_SKILL_*` while staying 100% compatible with `Result<T, String>` commands.

use std::collections::BTreeMap;
use std::fmt;

pub use odg_core::skills::{
    ERR_SKILL_BUILTIN, ERR_SKILL_CLONE_FAILED, ERR_SKILL_CLONE_TIMEOUT, ERR_SKILL_EXISTS, ERR_SKILL_GIT_MISSING,
    ERR_SKILL_INVALID_NAME, ERR_SKILL_INVALID_SOURCE, ERR_SKILL_NOT_FOUND, ERR_SKILL_PATH_ESCAPE, ERR_SKILL_SUBFOLDERS,
    ERR_SKILL_TOO_LARGE,
};
use serde::{Deserialize, Serialize};

pub const ERR_WORKTREE_BUSY: &str = "ERR_WORKTREE_BUSY";
pub const ERR_SESSION_BUSY: &str = "ERR_SESSION_BUSY";
pub const ERR_NO_WORKTREE: &str = "ERR_NO_WORKTREE";
pub const ERR_EMPTY_MESSAGE: &str = "ERR_EMPTY_MESSAGE";
pub const ERR_INVALID_WORKSPACE: &str = "ERR_INVALID_WORKSPACE";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpcError {
    pub code: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    pub message: String,
}

impl IpcError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            params: BTreeMap::new(),
            message: message.into(),
        }
    }

    pub fn with_param(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.params.insert(key.into(), value.into());
        self
    }

    pub fn to_coded_string(&self) -> String {
        if self.params.is_empty() {
            format!("[{}] {}", self.code, self.message)
        } else {
            let meta: String = self
                .params
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(";");
            format!("[{}] {meta} | {}", self.code, self.message)
        }
    }
}

impl fmt::Display for IpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_coded_string())
    }
}

impl From<IpcError> for String {
    fn from(err: IpcError) -> Self {
        err.to_coded_string()
    }
}

/// Prefix `message` with `[<CODE>]`, avoiding double-wrapping if `message` already carries an `[ERR_*]` prefix.
pub fn coded(code: &str, message: impl fmt::Display) -> String {
    let text = message.to_string();
    let trimmed = text.trim_start();
    if trimmed.starts_with("[ERR_") && trimmed.contains(']') {
        return text;
    }
    format!("[{code}] {text}")
}

/// Prefix `message` with `[<CODE>] k1=v1;k2=v2 | <message>` so `parseBackendError` extracts `params`.
pub fn coded_with_params<'a>(
    code: &str,
    params: impl IntoIterator<Item = (&'a str, &'a str)>,
    message: impl fmt::Display,
) -> String {
    let mut err = IpcError::new(code, message.to_string());
    for (k, v) in params {
        err.params.insert(k.to_string(), v.to_string());
    }
    err.to_coded_string()
}

/// Parse a bracket-coded error string (`[ERR_<CODE>] ...`) into an [`IpcError`].
pub fn parse_coded(raw: &str) -> Option<IpcError> {
    let trimmed = raw.trim();
    let rest = trimmed.strip_prefix('[')?;
    let close = rest.find(']')?;
    let code = &rest[..close];
    if !code.starts_with("ERR_")
        || !code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    {
        return None;
    }
    let body = rest[close + 1..].trim();
    let mut params = BTreeMap::new();
    let message = if let Some((meta, msg)) = body.split_once('|')
        && !meta.contains('\n')
        && meta.contains('=')
    {
        let mut valid = true;
        for pair in meta.split(';') {
            if let Some((k, v)) = pair.split_once('=') {
                let k = k.trim();
                if !k.is_empty() && k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
                    params.insert(k.to_string(), v.trim().to_string());
                } else {
                    valid = false;
                    break;
                }
            }
        }
        if valid && !params.is_empty() {
            msg.trim().to_string()
        } else {
            params.clear();
            body.to_string()
        }
    } else {
        body.to_string()
    };
    Some(IpcError {
        code: code.to_string(),
        params,
        message,
    })
}

use crate::model::AppState;
use anyhow::{Context, Result};
use std::fs;
use std::path::PathBuf;

pub fn state_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("crisppdf")
        .join("state.json")
}

pub fn load() -> AppState {
    let path = state_path();
    let Ok(contents) = fs::read_to_string(&path) else {
        return AppState::default();
    };
    serde_json::from_str(&contents).unwrap_or_default()
}

pub fn save(state: &AppState) -> Result<()> {
    let path = state_path();
    let parent = path.parent().context("state path has no parent")?;
    fs::create_dir_all(parent).context("could not create CrispPDF config folder")?;
    let serialized = serde_json::to_string_pretty(state)?;
    fs::write(&path, serialized).with_context(|| format!("could not write {}", path.display()))
}

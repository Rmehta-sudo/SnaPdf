use crate::model::AppState;
use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

const CONFIG_DIR_NAME: &str = "snapdf";
const LEGACY_CONFIG_DIR_NAMES: [&str; 2] = ["snappdf", "crisppdf"];

fn config_base() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn copy_missing_file(source: &Path, destination: &Path) -> Result<()> {
    if destination.exists() || !source.is_file() {
        return Ok(());
    }
    let temporary = destination.with_extension("migration.tmp");
    fs::copy(source, &temporary).with_context(|| {
        format!(
            "could not migrate {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    fs::rename(&temporary, destination).with_context(|| {
        format!(
            "could not finish migrating {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

fn migrate_legacy_config_at(base: &Path) -> Result<PathBuf> {
    let current = base.join(CONFIG_DIR_NAME);
    for legacy_name in LEGACY_CONFIG_DIR_NAMES {
        let legacy = base.join(legacy_name);
        if !legacy.is_dir() {
            continue;
        }

        if !current.exists() && fs::rename(&legacy, &current).is_ok() {
            continue;
        }

        fs::create_dir_all(&current)?;
        for file_name in ["state.json", "chat-credentials.json"] {
            copy_missing_file(&legacy.join(file_name), &current.join(file_name))?;
        }
    }
    Ok(current)
}

pub fn config_dir() -> PathBuf {
    let base = config_base();
    migrate_legacy_config_at(&base).unwrap_or_else(|error| {
        eprintln!("SnaPdf could not migrate its previous configuration: {error}");
        base.join(CONFIG_DIR_NAME)
    })
}

pub fn state_path() -> PathBuf {
    config_dir().join("state.json")
}

pub fn load() -> AppState {
    let path = state_path();
    let contents = fs::read_to_string(&path);
    let Ok(contents) = contents else {
        return AppState::default();
    };
    serde_json::from_str(&contents).unwrap_or_default()
}

pub fn save(state: &AppState) -> Result<()> {
    let path = state_path();
    let parent = path.parent().context("state path has no parent")?;
    fs::create_dir_all(parent).context("could not create SnaPdf config folder")?;
    let serialized = serde_json::to_string_pretty(state)?;
    fs::write(&path, serialized).with_context(|| format!("could not write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_config_base(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("snapdf-{label}-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn migrates_previous_brand_config_without_losing_files() {
        let base = temporary_config_base("migration");
        let legacy = base.join("snappdf");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("state.json"), b"state").unwrap();
        fs::write(legacy.join("chat-credentials.json"), b"credentials").unwrap();

        let current = migrate_legacy_config_at(&base).unwrap();

        assert_eq!(fs::read(current.join("state.json")).unwrap(), b"state");
        assert_eq!(
            fs::read(current.join("chat-credentials.json")).unwrap(),
            b"credentials"
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn migration_never_overwrites_newer_config() {
        let base = temporary_config_base("no-overwrite");
        let legacy = base.join("snappdf");
        let current = base.join("snapdf");
        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(&current).unwrap();
        fs::write(legacy.join("state.json"), b"old").unwrap();
        fs::write(current.join("state.json"), b"new").unwrap();

        migrate_legacy_config_at(&base).unwrap();

        assert_eq!(fs::read(current.join("state.json")).unwrap(), b"new");
        fs::remove_dir_all(base).unwrap();
    }
}

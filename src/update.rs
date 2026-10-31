//! Release checks against GitHub and in-place updates of the portable
//! AppImage. Package-manager and source installs are only told that a newer
//! release exists; they are never modified from inside the app.

use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const REPOSITORY: &str = "Rmehta-sudo/SnaPdf";
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub page_url: String,
    appimage_url: Option<String>,
    appimage_name: String,
    checksums_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    Checking,
    UpToDate,
    Available(Release),
    Installing(Release),
    Failed(String),
}

enum UpdateCommand {
    Check,
    Install(Release, PathBuf),
}

pub enum UpdateEvent {
    /// `Some` only when the latest release is newer than this build.
    Checked(Result<Option<Release>, String>),
    Installed(Result<(), String>),
}

pub struct UpdateWorker {
    commands: Sender<UpdateCommand>,
    pub events: Receiver<UpdateEvent>,
}

impl UpdateWorker {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (commands, command_receiver) = unbounded();
        let (event_sender, events) = unbounded();
        std::thread::Builder::new()
            .name("snapdf-update".to_owned())
            .spawn(move || {
                for command in command_receiver {
                    let event = match command {
                        UpdateCommand::Check => UpdateEvent::Checked(check_latest()),
                        UpdateCommand::Install(release, target) => {
                            UpdateEvent::Installed(install(&release, &target))
                        }
                    };
                    if event_sender.send(event).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            })
            .ok();
        Self { commands, events }
    }

    pub fn check(&self) {
        let _ = self.commands.send(UpdateCommand::Check);
    }

    pub fn install(&self, release: Release, target: PathBuf) {
        let _ = self.commands.send(UpdateCommand::Install(release, target));
    }
}

/// The AppImage this process was started from, when it can replace itself.
pub fn self_update_target() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("APPIMAGE")?);
    path.is_file().then_some(path)
}

/// The command that starts SnaPdf again after an update: the installer's
/// wrapper when present, otherwise the AppImage itself.
pub fn relaunch_command() -> Option<PathBuf> {
    std::env::var_os("SNAPDF_EXECUTABLE")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(self_update_target)
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(format!("SnaPdf/{CURRENT_VERSION}"))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|error| error.to_string())
}

fn check_latest() -> Result<Option<Release>, String> {
    let response = client()?
        .get(format!(
            "https://api.github.com/repos/{REPOSITORY}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|error| format!("could not reach GitHub ({error})"))?;
    let body: serde_json::Value = response
        .json()
        .map_err(|error| format!("unexpected release data ({error})"))?;
    let release = parse_release(&body).ok_or("unexpected release data")?;
    Ok(is_newer(&release.version, CURRENT_VERSION).then_some(release))
}

fn parse_release(body: &serde_json::Value) -> Option<Release> {
    let tag = body.get("tag_name")?.as_str()?;
    let version = tag.trim_start_matches('v').to_owned();
    let page_url = body
        .get("html_url")
        .and_then(|url| url.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("https://github.com/{REPOSITORY}/releases/tag/{tag}"));
    let appimage_name = format!("SnaPdf-{version}-x86_64.AppImage");
    let asset_url = |name: &str| {
        body.get("assets")?
            .as_array()?
            .iter()
            .find(|asset| asset.get("name").and_then(|n| n.as_str()) == Some(name))?
            .get("browser_download_url")?
            .as_str()
            .map(str::to_owned)
    };
    Some(Release {
        appimage_url: asset_url(&appimage_name),
        checksums_url: asset_url("SHA256SUMS"),
        version,
        page_url,
        appimage_name,
    })
}

fn version_parts(version: &str) -> Vec<u64> {
    version
        .split(['-', '+'])
        .next()
        .unwrap_or_default()
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (mut candidate, mut current) = (version_parts(candidate), version_parts(current));
    let width = candidate.len().max(current.len());
    candidate.resize(width, 0);
    current.resize(width, 0);
    candidate > current
}

/// Download the release AppImage beside `target`, verify it against the
/// release's SHA256SUMS, and atomically rename it over `target`. The running
/// process keeps its already-extracted copy, so replacing the file is safe.
fn install(release: &Release, target: &Path) -> Result<(), String> {
    let (Some(appimage_url), Some(checksums_url)) = (&release.appimage_url, &release.checksums_url)
    else {
        return Err("this release has no AppImage for this computer".to_owned());
    };
    let client = client()?;
    let checksums = client
        .get(checksums_url)
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.text())
        .map_err(|error| format!("could not download checksums ({error})"))?;
    let expected = expected_checksum(&checksums, &release.appimage_name)
        .ok_or("the release checksums do not list its AppImage")?;

    let directory = target.parent().ok_or("invalid install location")?;
    let partial = directory.join(".SnaPdf.AppImage.download");
    let result = (|| {
        let mut response = client
            .get(appimage_url)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| format!("could not download the update ({error})"))?;
        let mut file = std::fs::File::create(&partial)
            .map_err(|error| format!("could not write to {} ({error})", directory.display()))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = response
                .read(&mut buffer)
                .map_err(|error| format!("download interrupted ({error})"))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .map_err(|error| format!("could not save the update ({error})"))?;
        }
        file.sync_all().map_err(|error| error.to_string())?;
        let actual = format!("{:x}", hasher.finalize());
        if actual != expected {
            return Err("the downloaded update failed checksum verification".to_owned());
        }
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o755))
            .map_err(|error| error.to_string())?;
        std::fs::rename(&partial, target)
            .map_err(|error| format!("could not replace {} ({error})", target.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

fn expected_checksum(checksums: &str, asset: &str) -> Option<String> {
    checksums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let digest = fields.next()?;
        let name = fields.next()?.trim_start_matches('*');
        (name == asset).then(|| digest.to_ascii_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("0.4.0", "0.3.5"));
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("1.0", "0.99.99"));
        assert!(!is_newer("0.3.5", "0.3.5"));
        assert!(!is_newer("0.3.4", "0.3.5"));
        assert!(!is_newer("0.3.5-rc1", "0.3.5"));
    }

    #[test]
    fn release_assets_are_found_by_exact_name() {
        let body = serde_json::json!({
            "tag_name": "v0.4.0",
            "html_url": "https://example.invalid/release",
            "assets": [
                {"name": "SHA256SUMS", "browser_download_url": "https://example.invalid/sums"},
                {"name": "SnaPdf-0.4.0-x86_64.AppImage", "browser_download_url": "https://example.invalid/app"},
            ],
        });
        let release = parse_release(&body).unwrap();
        assert_eq!(release.version, "0.4.0");
        assert_eq!(
            release.appimage_url.as_deref(),
            Some("https://example.invalid/app")
        );
        assert_eq!(
            release.checksums_url.as_deref(),
            Some("https://example.invalid/sums")
        );
    }

    #[test]
    fn checksum_lines_match_the_asset_name() {
        let sums = "abc123  install-snapdf.sh\nDEF456 *SnaPdf-0.4.0-x86_64.AppImage\n";
        assert_eq!(
            expected_checksum(sums, "SnaPdf-0.4.0-x86_64.AppImage").as_deref(),
            Some("def456")
        );
        assert_eq!(expected_checksum(sums, "missing"), None);
    }
}

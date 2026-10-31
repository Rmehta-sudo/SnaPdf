//! Single-window handoff. The first SnaPdf process owns a per-user Unix socket;
//! later launches (file managers, browsers, `snapdf file.pdf`) forward their
//! canonical targets to it and exit, so every PDF becomes a tab in one window.

use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui;
use std::ffi::OsStr;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

const ACK: u8 = 0x06;
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchTarget {
    Pdf(PathBuf),
    Project(PathBuf),
}

pub enum Claim {
    /// This process owns the window. The listener is `None` only when the
    /// socket could not be created; SnaPdf then still runs as a lone window.
    Primary(Option<UnixListener>),
    Forwarded,
}

pub fn socket_path() -> PathBuf {
    dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("snapdf")
        .join("instance.sock")
}

/// Hand `targets` to a running window, or become the window that later
/// launches hand off to. An empty target list asks the running window to come
/// to the front.
pub fn claim(socket: &Path, targets: &[LaunchTarget]) -> Claim {
    if forward(socket, targets).is_ok() {
        return Claim::Forwarded;
    }
    if let Some(parent) = socket.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match UnixListener::bind(socket) {
        Ok(listener) => Claim::Primary(Some(listener)),
        Err(error) if error.kind() == ErrorKind::AddrInUse => {
            // Either a crashed process left the file behind, or another launch
            // won the race a moment ago. Retry the handoff before reclaiming.
            if forward(socket, targets).is_ok() {
                return Claim::Forwarded;
            }
            let _ = std::fs::remove_file(socket);
            Claim::Primary(UnixListener::bind(socket).ok())
        }
        Err(_) => Claim::Primary(None),
    }
}

fn forward(socket: &Path, targets: &[LaunchTarget]) -> std::io::Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_write_timeout(Some(HANDOFF_TIMEOUT))?;
    stream.set_read_timeout(Some(HANDOFF_TIMEOUT))?;
    stream.write_all(&encode(targets))?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut ack = [0u8; 1];
    stream.read_exact(&mut ack)?;
    if ack[0] == ACK {
        Ok(())
    } else {
        Err(ErrorKind::InvalidData.into())
    }
}

/// Accept handoffs on a background thread. Each accepted connection yields
/// one batch of targets and wakes the UI.
pub fn listen(listener: UnixListener, ctx: egui::Context) -> Receiver<Vec<LaunchTarget>> {
    let (sender, receiver) = unbounded();
    std::thread::Builder::new()
        .name("snapdf-instance".to_owned())
        .spawn(move || serve(listener, sender, ctx))
        .ok();
    receiver
}

fn serve(listener: UnixListener, sender: Sender<Vec<LaunchTarget>>, ctx: egui::Context) {
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(HANDOFF_TIMEOUT));
        let mut bytes = Vec::new();
        if stream.read_to_end(&mut bytes).is_err() {
            continue;
        }
        let _ = stream.write_all(&[ACK]);
        if sender.send(decode(&bytes)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

/// Each target is a kind byte (`P`df or `D`irectory), the raw path bytes, and
/// a NUL terminator. Paths are absolute, so NUL can never occur inside one.
fn encode(targets: &[LaunchTarget]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for target in targets {
        let (kind, path) = match target {
            LaunchTarget::Pdf(path) => (b'P', path),
            LaunchTarget::Project(path) => (b'D', path),
        };
        bytes.push(kind);
        bytes.extend_from_slice(path.as_os_str().as_bytes());
        bytes.push(0);
    }
    bytes
}

fn decode(bytes: &[u8]) -> Vec<LaunchTarget> {
    bytes
        .split(|byte| *byte == 0)
        .filter_map(|record| {
            let (kind, path) = record.split_first()?;
            let path = PathBuf::from(OsStr::from_bytes(path));
            // The sender canonicalized these; re-check them on this side so a
            // stray connection can only ever open existing PDFs or folders.
            match kind {
                b'P' if path.is_absolute() && path.is_file() => Some(LaunchTarget::Pdf(path)),
                b'D' if path.is_absolute() && path.is_dir() => Some(LaunchTarget::Project(path)),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_socket(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("snapdf-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("instance.sock")
    }

    #[test]
    fn targets_round_trip_through_the_wire_format() {
        let dir = std::env::current_dir().unwrap().canonicalize().unwrap();
        let file = dir.join("Cargo.toml");
        let targets = vec![
            LaunchTarget::Project(dir.clone()),
            LaunchTarget::Pdf(file.clone()),
            LaunchTarget::Pdf(dir.join("missing.pdf")),
        ];
        assert_eq!(
            decode(&encode(&targets)),
            vec![LaunchTarget::Project(dir), LaunchTarget::Pdf(file)]
        );
    }

    #[test]
    fn second_launch_forwards_to_the_first_window() {
        let socket = temp_socket("forward");
        let Claim::Primary(Some(listener)) = claim(&socket, &[]) else {
            panic!("first launch must own the socket");
        };
        let ctx = egui::Context::default();
        let receiver = listen(listener, ctx);
        let dir = std::env::current_dir().unwrap().canonicalize().unwrap();
        let targets = vec![LaunchTarget::Project(dir)];
        assert!(matches!(claim(&socket, &targets), Claim::Forwarded));
        assert_eq!(receiver.recv_timeout(HANDOFF_TIMEOUT).unwrap(), targets);
    }

    #[test]
    fn stale_socket_from_a_crashed_window_is_reclaimed() {
        let socket = temp_socket("stale");
        drop(UnixListener::bind(&socket).unwrap());
        assert!(socket.exists());
        assert!(matches!(claim(&socket, &[]), Claim::Primary(Some(_))));
    }
}

use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::json;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

#[derive(Debug)]
pub enum OcrCommand {
    Warmup,
    Recognize {
        request_id: u64,
        width: usize,
        height: usize,
        rgb: Vec<u8>,
        math: bool,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum OcrEvent {
    Ready,
    Result {
        request_id: u64,
        text: String,
        engine: String,
        elapsed_ms: u128,
    },
    Error {
        request_id: Option<u64>,
        message: String,
    },
}

pub struct OcrWorker {
    pub commands: Sender<OcrCommand>,
    pub events: Receiver<OcrEvent>,
}

impl OcrWorker {
    pub fn spawn(repaint: eframe::egui::Context) -> Self {
        let (command_tx, command_rx) = unbounded();
        let (event_tx, event_rx) = unbounded();
        std::thread::Builder::new()
            .name("snapdf-ocr".to_owned())
            .spawn(move || worker_loop(command_rx, event_tx, repaint))
            .expect("could not start OCR worker thread");
        Self {
            commands: command_tx,
            events: event_rx,
        }
    }
}

struct OcrProcess {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl OcrProcess {
    fn start() -> Result<Self, String> {
        let root = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("snapdf")
            .join("ocr");
        let python = root.join("venv/bin/python");
        let script = root.join("ocr_worker.py");
        if !python.is_file() || !script.is_file() {
            return Err("Local OCR is not installed. Run scripts/install-ocr.sh from the SnaPdf project once.".to_owned());
        }
        let mut child = Command::new(python)
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("Could not start local OCR: {error}"))?;
        let input = child.stdin.take().ok_or("OCR input pipe unavailable")?;
        let output = BufReader::new(child.stdout.take().ok_or("OCR output pipe unavailable")?);
        Ok(Self {
            child,
            input,
            output,
        })
    }

    fn request(&mut self, payload: serde_json::Value) -> Result<serde_json::Value, String> {
        serde_json::to_writer(&mut self.input, &payload)
            .map_err(|error| format!("Could not send OCR request: {error}"))?;
        self.input
            .write_all(b"\n")
            .and_then(|_| self.input.flush())
            .map_err(|error| format!("Could not send OCR request: {error}"))?;
        let mut line = String::new();
        self.output
            .read_line(&mut line)
            .map_err(|error| format!("Could not read OCR result: {error}"))?;
        if line.trim().is_empty() {
            return Err("Local OCR stopped unexpectedly.".to_owned());
        }
        serde_json::from_str(&line).map_err(|error| format!("Invalid OCR result: {error}"))
    }
}

impl Drop for OcrProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn worker_loop(
    commands: Receiver<OcrCommand>,
    events: Sender<OcrEvent>,
    repaint: eframe::egui::Context,
) {
    let mut process: Option<OcrProcess> = None;
    while let Ok(command) = commands.recv() {
        match command {
            OcrCommand::Shutdown => break,
            OcrCommand::Warmup => {
                let result = (|| -> Result<serde_json::Value, String> {
                    if process.is_none() {
                        process = Some(OcrProcess::start()?);
                    }
                    process
                        .as_mut()
                        .unwrap()
                        .request(json!({"command": "warmup"}))
                })();
                match result {
                    Ok(_) => {
                        let _ = events.send(OcrEvent::Ready);
                    }
                    Err(message) => {
                        let _ = events.send(OcrEvent::Error {
                            request_id: None,
                            message,
                        });
                    }
                }
                repaint.request_repaint();
            }
            OcrCommand::Recognize {
                request_id,
                width,
                height,
                rgb,
                math,
            } => {
                let start = Instant::now();
                let result = (|| -> Result<(String, String), String> {
                    if process.is_none() {
                        process = Some(OcrProcess::start()?);
                    }
                    let cache = dirs::cache_dir()
                        .unwrap_or_else(std::env::temp_dir)
                        .join("snapdf");
                    fs::create_dir_all(&cache)
                        .map_err(|error| format!("Could not create OCR cache: {error}"))?;
                    let path = cache.join(format!("snip-{}-{request_id}.ppm", std::process::id()));
                    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
                    ppm.extend_from_slice(&rgb);
                    fs::write(&path, ppm)
                        .map_err(|error| format!("Could not prepare OCR crop: {error}"))?;
                    let response = process.as_mut().unwrap().request(json!({
                        "command": "recognize",
                        "path": path,
                        "mode": if math { "math" } else { "text" },
                    }));
                    let _ = fs::remove_file(&path);
                    let response = response?;
                    if let Some(error) = response.get("error").and_then(|value| value.as_str()) {
                        return Err(error.to_owned());
                    }
                    Ok((
                        response
                            .get("text")
                            .and_then(|value| value.as_str())
                            .unwrap_or_default()
                            .to_owned(),
                        response
                            .get("engine")
                            .and_then(|value| value.as_str())
                            .unwrap_or("PaddleOCR")
                            .to_owned(),
                    ))
                })();
                match result {
                    Ok((text, engine)) => {
                        let _ = events.send(OcrEvent::Result {
                            request_id,
                            text,
                            engine,
                            elapsed_ms: start.elapsed().as_millis(),
                        });
                    }
                    Err(message) => {
                        let _ = events.send(OcrEvent::Error {
                            request_id: Some(request_id),
                            message,
                        });
                    }
                }
                repaint.request_repaint();
            }
        }
    }
}

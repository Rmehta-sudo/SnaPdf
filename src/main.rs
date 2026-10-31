mod app;
mod chat;
mod gestures;
mod instance;
mod model;
mod ocr;
mod pdf;
mod storage;
mod theme;
mod update;

use anyhow::Result;

fn main() -> Result<()> {
    app::run()
}

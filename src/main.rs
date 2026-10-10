mod app;
mod model;
mod ocr;
mod pdf;
mod storage;
mod theme;

use anyhow::Result;

fn main() -> Result<()> {
    app::run()
}

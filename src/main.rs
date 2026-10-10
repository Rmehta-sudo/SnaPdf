mod app;
mod model;
mod pdf;
mod storage;
mod theme;

use anyhow::Result;

fn main() -> Result<()> {
    app::run()
}

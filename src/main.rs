mod app;
mod tree;
mod ui;

use anyhow::Result;
use std::{env, process};

fn main() -> Result<()> {
    let path = env::args().nth(1).unwrap_or_else(|| {
        eprintln!("Usage: tuitr <file|directory>");
        process::exit(1);
    });

    app::App::new(path)?.run()
}

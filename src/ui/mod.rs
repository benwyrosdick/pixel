mod canvas;
mod dialogs;
mod layers;
mod model;
mod shell;
mod theme;

use std::path::PathBuf;

pub fn present(app: &libadwaita::Application, open: Option<PathBuf>) {
    shell::present(app, open);
}

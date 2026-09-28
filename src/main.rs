mod ui;

use gtk::prelude::*;
use gtk::{gio, glib};

fn main() -> glib::ExitCode {
    gio::resources_register_include!("pixel.gresource").expect("the bundled icons are valid");
    let app = libadwaita::Application::builder()
        .application_id("app.pixel.Pixel")
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();

    app.connect_activate(|app| ui::present(app, None));
    app.connect_open(|app, files, _| {
        let path = files.first().and_then(|file| file.path());
        ui::present(app, path);
    });

    app.run()
}

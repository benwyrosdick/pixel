//! Modal dialogs for canvas setup, resize, rotate, adjustments, and export.

use gtk::prelude::*;
use gtk::{gdk, glib};
use pixel::document::{
    Anchor, Background, ExportFormat, NewCanvas, ScaleFilter, TextAlign, TextSpec, MAX_EDGE,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub fn new_canvas(parent: &impl IsA<gtk::Window>, on_ok: impl Fn(NewCanvas) + 'static) {
    let presets: &[(&str, u32, u32, f32)] = &[
        ("1920 × 1080", 1920, 1080, 72.0),
        ("1080 × 1080", 1080, 1080, 72.0),
        ("1080 × 1920", 1080, 1920, 72.0),
        ("A4 at 300 PPI", 2480, 3508, 300.0),
    ];
    let mut names = vec!["Custom"];
    names.extend(presets.iter().map(|(name, _, _, _)| *name));
    let preset = gtk::DropDown::from_strings(&names);
    preset.set_selected(1);

    let width = spin(1920.0);
    let height = spin(1080.0);
    let ppi = spin(72.0);
    ppi.set_range(1.0, 1200.0);

    let background = gtk::DropDown::from_strings(&["Transparent", "White", "Black", "Custom"]);
    let color = gtk::ColorDialogButton::builder()
        .dialog(&gtk::ColorDialog::new())
        .rgba(&gdk::RGBA::new(1.0, 1.0, 1.0, 1.0))
        .build();
    color.set_visible(false);

    let guard = Rc::new(Cell::new(false));
    let width_c = width.clone();
    let height_c = height.clone();
    let ppi_c = ppi.clone();
    let guard_c = guard.clone();
    preset.connect_selected_notify(move |preset| {
        let index = preset.selected();
        if index == 0 || index as usize > presets.len() {
            return;
        }
        let (_, w, h, p) = presets[index as usize - 1];
        guard_c.set(true);
        width_c.set_value(w as f64);
        height_c.set_value(h as f64);
        ppi_c.set_value(p as f64);
        guard_c.set(false);
    });
    for spin_btn in [&width, &height, &ppi] {
        let preset = preset.clone();
        let guard = guard.clone();
        spin_btn.connect_value_changed(move |_| {
            if !guard.get() {
                preset.set_selected(0);
            }
        });
    }
    let color_row = color.clone();
    background.connect_selected_notify(move |menu| {
        color_row.set_visible(menu.selected() == 3);
    });

    let dialog = form_window(parent, "New canvas");
    let form = gtk::Box::new(gtk::Orientation::Vertical, 8);
    form.set_margin_top(16);
    form.set_margin_bottom(16);
    form.set_margin_start(16);
    form.set_margin_end(16);
    form.append(&labeled("Preset", &preset));
    form.append(&labeled("Width", &width));
    form.append(&labeled("Height", &height));
    form.append(&labeled("PPI", &ppi));
    form.append(&labeled("Background", &background));
    form.append(&labeled("Color", &color));

    let on_ok = Rc::new(RefCell::new(Some(on_ok)));
    let dialog_ok = dialog.clone();
    let create = gtk::Button::with_label("Create");
    create.add_css_class("suggested-action");
    create.connect_clicked(move |_| {
        let spec = NewCanvas {
            width: spin_u32(&width),
            height: spin_u32(&height),
            ppi: ppi.value() as f32,
            background: match background.selected() {
                1 => Background::white(),
                2 => Background::black(),
                3 => Background::Solid(rgba_bytes(color.rgba())),
                _ => Background::Transparent,
            },
        };
        dialog_ok.close();
        if let Some(callback) = on_ok.borrow_mut().take() {
            callback(spec);
        }
    });
    let cancel = cancel_button(&dialog);
    form.append(&button_row(&cancel, &create));
    dialog.set_default_widget(Some(&create));
    dialog.set_child(Some(&form));
    dialog.present();
}

pub fn canvas_size(
    parent: &impl IsA<gtk::Window>,
    current_w: u32,
    current_h: u32,
    on_ok: impl Fn(u32, u32, Anchor) + 'static,
) {
    let width = spin(current_w as f64);
    let height = spin(current_h as f64);
    let (anchor_grid, anchor) = anchor_picker();
    let dialog = form_window(parent, "Canvas size");
    let form = padded_column();
    form.append(&gtk::Label::new(Some(
        "Changes the canvas bounds without resampling layers.",
    )));
    form.append(&labeled("Width", &width));
    form.append(&labeled("Height", &height));
    form.append(&labeled("Anchor", &anchor_grid));
    let on_ok = Rc::new(RefCell::new(Some(on_ok)));
    let dialog_ok = dialog.clone();
    let apply = gtk::Button::with_label("Resize");
    apply.add_css_class("suggested-action");
    apply.connect_clicked(move |_| {
        let anchor = anchor.get();
        let (w, h) = (spin_u32(&width), spin_u32(&height));
        dialog_ok.close();
        if let Some(callback) = on_ok.borrow_mut().take() {
            callback(w, h, anchor);
        }
    });
    form.append(&button_row(&cancel_button(&dialog), &apply));
    dialog.set_default_widget(Some(&apply));
    dialog.set_child(Some(&form));
    dialog.present();
}

pub fn image_size(
    parent: &impl IsA<gtk::Window>,
    current_w: u32,
    current_h: u32,
    on_ok: impl Fn(u32, u32, ScaleFilter) + 'static,
) {
    let width = spin(current_w as f64);
    let height = spin(current_h as f64);
    let aspect = Rc::new(Cell::new(current_h as f64 / current_w as f64));
    let lock = gtk::CheckButton::with_label("Lock aspect ratio");
    lock.set_active(true);
    let guard = Rc::new(Cell::new(false));
    let height_c = height.clone();
    let aspect_c = aspect.clone();
    let lock_c = lock.clone();
    let guard_c = guard.clone();
    width.connect_value_changed(move |width| {
        if guard_c.get() || !lock_c.is_active() {
            return;
        }
        guard_c.set(true);
        height_c.set_value((width.value() * aspect_c.get()).clamp(1.0, MAX_EDGE as f64));
        guard_c.set(false);
    });
    let width_c = width.clone();
    let aspect_c = aspect.clone();
    let lock_c = lock.clone();
    let guard_c = guard.clone();
    height.connect_value_changed(move |height| {
        if guard_c.get() || !lock_c.is_active() {
            return;
        }
        guard_c.set(true);
        let ratio = aspect_c.get();
        if ratio > 0.0 {
            width_c.set_value((height.value() / ratio).clamp(1.0, MAX_EDGE as f64));
        }
        guard_c.set(false);
    });
    let width_c = width.clone();
    let height_c = height.clone();
    lock.connect_toggled(move |lock| {
        if lock.is_active() && width_c.value() > 0.0 {
            aspect.set(height_c.value() / width_c.value());
        }
    });

    let filter = gtk::DropDown::from_strings(&["Nearest", "Triangle", "Lanczos3"]);
    filter.set_selected(2);
    let dialog = form_window(parent, "Image size");
    let form = padded_column();
    form.append(&gtk::Label::new(Some(
        "Resamples every layer and the canvas together.",
    )));
    form.append(&labeled("Width", &width));
    form.append(&labeled("Height", &height));
    form.append(&lock);
    form.append(&labeled("Filter", &filter));
    let on_ok = Rc::new(RefCell::new(Some(on_ok)));
    let dialog_ok = dialog.clone();
    let apply = gtk::Button::with_label("Scale");
    apply.add_css_class("suggested-action");
    apply.connect_clicked(move |_| {
        let filter = match filter.selected() {
            0 => ScaleFilter::Nearest,
            1 => ScaleFilter::Triangle,
            _ => ScaleFilter::Lanczos3,
        };
        let (w, h) = (spin_u32(&width), spin_u32(&height));
        dialog_ok.close();
        if let Some(callback) = on_ok.borrow_mut().take() {
            callback(w, h, filter);
        }
    });
    form.append(&button_row(&cancel_button(&dialog), &apply));
    dialog.set_default_widget(Some(&apply));
    dialog.set_child(Some(&form));
    dialog.present();
}

pub fn rotate_layer(parent: &impl IsA<gtk::Window>, on_ok: impl Fn(f32) + 'static) {
    let degrees = gtk::SpinButton::with_range(-360.0, 360.0, 1.0);
    degrees.set_digits(1);
    degrees.set_value(90.0);
    let dialog = form_window(parent, "Rotate layer");
    let form = padded_column();
    form.append(&gtk::Label::new(Some(
        "Clockwise degrees. The layer grows so the corners stay inside it.",
    )));
    form.append(&labeled("Degrees", &degrees));
    let on_ok = Rc::new(RefCell::new(Some(on_ok)));
    let dialog_ok = dialog.clone();
    let apply = gtk::Button::with_label("Rotate");
    apply.add_css_class("suggested-action");
    apply.connect_clicked(move |_| {
        let value = degrees.value() as f32;
        dialog_ok.close();
        if let Some(callback) = on_ok.borrow_mut().take() {
            callback(value);
        }
    });
    form.append(&button_row(&cancel_button(&dialog), &apply));
    dialog.set_default_widget(Some(&apply));
    dialog.set_child(Some(&form));
    dialog.present();
}

/// Write new text, or change a text layer's. `confirm` labels the button.
pub fn text(
    parent: &impl IsA<gtk::Window>,
    title: &str,
    confirm: &str,
    initial: &TextSpec,
    on_ok: impl Fn(TextSpec) + 'static,
) {
    let dialog = form_window(parent, title);
    let form = padded_column();

    let buffer = gtk::TextBuffer::new(None);
    buffer.set_text(&initial.text);
    let view = gtk::TextView::with_buffer(&buffer);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_accepts_tab(false);
    for set_margin in [
        gtk::TextView::set_top_margin,
        gtk::TextView::set_bottom_margin,
        gtk::TextView::set_left_margin,
        gtk::TextView::set_right_margin,
    ] {
        set_margin(&view, 8);
    }
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_min_content_height(96);
    scroller.set_child(Some(&view));
    scroller.add_css_class("card");
    form.append(&scroller);

    let font = gtk::FontDialogButton::new(Some(gtk::FontDialog::new()));
    // The size is its own field, in pixels, so pick only the face here.
    font.set_level(gtk::FontLevel::Face);
    font.set_font_desc(&gtk::pango::FontDescription::from_string(&initial.font));
    font.set_hexpand(true);
    form.append(&labeled("Font", &font));

    let size = gtk::SpinButton::with_range(4.0, 1000.0, 1.0);
    size.set_digits(0);
    size.set_value(initial.size as f64);
    size.set_hexpand(true);
    form.append(&labeled("Size", &size));

    let color = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
    color.set_rgba(&rgba_color(initial.color));
    color.set_hexpand(true);
    form.append(&labeled("Color", &color));

    let align = gtk::DropDown::from_strings(&["Left", "Center", "Right"]);
    align.set_selected(match initial.align {
        TextAlign::Left => 0,
        TextAlign::Center => 1,
        TextAlign::Right => 2,
    });
    align.set_hexpand(true);
    form.append(&labeled("Align", &align));

    let apply = gtk::Button::with_label(confirm);
    apply.add_css_class("suggested-action");
    let dialog_ok = dialog.clone();
    apply.connect_clicked(move |_| {
        let mut face = font
            .font_desc()
            .unwrap_or_else(|| gtk::pango::FontDescription::from_string("Sans"));
        face.unset_fields(gtk::pango::FontMask::SIZE);
        let (start, end) = buffer.bounds();
        let spec = TextSpec {
            text: buffer.text(&start, &end, false).to_string(),
            font: face.to_string(),
            size: size.value() as f32,
            color: rgba_bytes(color.rgba()),
            align: match align.selected() {
                1 => TextAlign::Center,
                2 => TextAlign::Right,
                _ => TextAlign::Left,
            },
        };
        dialog_ok.close();
        on_ok(spec);
    });
    form.append(&button_row(&cancel_button(&dialog), &apply));
    dialog.set_child(Some(&form));
    dialog.present();
    view.grab_focus();
}

/// One slider in an adjustment dialog.
pub struct Slider {
    pub label: &'static str,
    pub min: f64,
    pub max: f64,
    pub value: f64,
    /// Decimal places shown and stepped.
    pub digits: u32,
}

/// A dialog of sliders for an adjustment or filter. `preview` runs with the
/// slider values when it opens and whenever one moves, `on_ok` when the change
/// is applied, and `on_cancel` when the dialog closes any other way.
pub fn adjustment(
    parent: &impl IsA<gtk::Window>,
    title: &str,
    sliders: &[Slider],
    preview: impl Fn(&[f64]) + 'static,
    on_ok: impl Fn(&[f64]) + 'static,
    on_cancel: impl Fn() + 'static,
) {
    let dialog = form_window(parent, title);
    let form = padded_column();
    let scales: Vec<gtk::Scale> = sliders
        .iter()
        .map(|slider| {
            let step = 10f64.powi(-(slider.digits as i32));
            let scale =
                gtk::Scale::with_range(gtk::Orientation::Horizontal, slider.min, slider.max, step);
            scale.set_digits(slider.digits as i32);
            scale.set_draw_value(true);
            scale.set_value_pos(gtk::PositionType::Right);
            scale.set_value(slider.value);
            scale.set_hexpand(true);
            form.append(&labeled(slider.label, &scale));
            scale
        })
        .collect();
    let values = {
        let scales = scales.clone();
        move || scales.iter().map(|scale| scale.value()).collect::<Vec<_>>()
    };
    let preview = Rc::new(preview);
    for scale in &scales {
        let preview = preview.clone();
        let values = values.clone();
        scale.connect_value_changed(move |_| preview(&values()));
    }

    let applied = Rc::new(Cell::new(false));
    let apply = gtk::Button::with_label("Apply");
    apply.add_css_class("suggested-action");
    let dialog_ok = dialog.clone();
    let applied_ok = applied.clone();
    let apply_values = values.clone();
    apply.connect_clicked(move |_| {
        applied_ok.set(true);
        on_ok(&apply_values());
        dialog_ok.close();
    });
    dialog.connect_close_request(move |_| {
        if !applied.get() {
            on_cancel();
        }
        glib::Propagation::Proceed
    });
    form.append(&button_row(&cancel_button(&dialog), &apply));
    dialog.set_default_widget(Some(&apply));
    dialog.set_child(Some(&form));
    dialog.present();
    preview(&values());
}

pub fn export_options(
    parent: &impl IsA<gtk::Window>,
    flattens_on_white: bool,
    on_ok: impl Fn(ExportFormat) + 'static,
) {
    let format = gtk::DropDown::from_strings(&["PNG", "JPEG", "WebP"]);
    let quality = gtk::Scale::with_range(gtk::Orientation::Horizontal, 1.0, 100.0, 1.0);
    quality.set_value(90.0);
    quality.set_hexpand(true);
    let quality_row = labeled("Quality", &quality);
    quality_row.set_visible(false);
    let note = gtk::Label::new(Some(
        "JPEG has no transparency, so this flattens onto white.",
    ));
    note.set_wrap(true);
    note.set_visible(false);
    let note_c = note.clone();
    let quality_c = quality_row.clone();
    let flattens = flattens_on_white;
    format.connect_selected_notify(move |format| {
        let jpeg = format.selected() == 1;
        quality_c.set_visible(jpeg);
        note_c.set_visible(jpeg && flattens);
    });

    let dialog = form_window(parent, "Export");
    let form = padded_column();
    form.append(&labeled("Format", &format));
    form.append(&quality_row);
    form.append(&note);
    let on_ok = Rc::new(RefCell::new(Some(on_ok)));
    let dialog_ok = dialog.clone();
    let apply = gtk::Button::with_label("Choose file");
    apply.add_css_class("suggested-action");
    apply.connect_clicked(move |_| {
        let format = match format.selected() {
            1 => ExportFormat::Jpeg {
                quality: quality.value().round().clamp(1.0, 100.0) as u8,
            },
            2 => ExportFormat::Webp,
            _ => ExportFormat::Png,
        };
        dialog_ok.close();
        if let Some(callback) = on_ok.borrow_mut().take() {
            callback(format);
        }
    });
    form.append(&button_row(&cancel_button(&dialog), &apply));
    dialog.set_default_widget(Some(&apply));
    dialog.set_child(Some(&form));
    dialog.present();
}

fn form_window(parent: &impl IsA<gtk::Window>, title: &str) -> gtk::Window {
    gtk::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(title)
        .default_width(380)
        .build()
}

fn padded_column() -> gtk::Box {
    let form = gtk::Box::new(gtk::Orientation::Vertical, 8);
    form.set_margin_top(16);
    form.set_margin_bottom(16);
    form.set_margin_start(16);
    form.set_margin_end(16);
    form
}

fn spin(value: f64) -> gtk::SpinButton {
    let spin = gtk::SpinButton::with_range(1.0, MAX_EDGE as f64, 1.0);
    spin.set_digits(0);
    spin.set_value(value);
    spin.set_hexpand(true);
    spin
}

fn spin_u32(spin: &gtk::SpinButton) -> u32 {
    spin.value().round().clamp(1.0, MAX_EDGE as f64) as u32
}

fn labeled(text: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_width_request(96);
    row.append(&label);
    row.append(widget);
    row
}

fn button_row(cancel: &gtk::Button, confirm: &gtk::Button) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::End);
    row.set_margin_top(8);
    row.append(cancel);
    row.append(confirm);
    row
}

fn cancel_button(dialog: &gtk::Window) -> gtk::Button {
    let button = gtk::Button::with_label("Cancel");
    let dialog = dialog.clone();
    button.connect_clicked(move |_| dialog.close());
    button
}

fn anchor_picker() -> (gtk::Grid, Rc<Cell<Anchor>>) {
    let grid = gtk::Grid::new();
    grid.set_column_spacing(4);
    grid.set_row_spacing(4);
    grid.set_halign(gtk::Align::Start);
    let selected = Rc::new(Cell::new(Anchor::Center));
    let anchors = [
        (0, 0, Anchor::NorthWest),
        (1, 0, Anchor::North),
        (2, 0, Anchor::NorthEast),
        (0, 1, Anchor::West),
        (1, 1, Anchor::Center),
        (2, 1, Anchor::East),
        (0, 2, Anchor::SouthWest),
        (1, 2, Anchor::South),
        (2, 2, Anchor::SouthEast),
    ];
    let mut buttons = Vec::new();
    for (column, row, anchor) in anchors {
        let button = gtk::ToggleButton::new();
        button.set_size_request(28, 28);
        if anchor == Anchor::Center {
            button.set_active(true);
        }
        grid.attach(&button, column, row, 1, 1);
        buttons.push((button, anchor));
    }
    let first = buttons[0].0.clone();
    for (button, _) in buttons.iter().skip(1) {
        button.set_group(Some(&first));
    }
    for (button, anchor) in buttons {
        let selected = selected.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                selected.set(anchor);
            }
        });
    }
    (grid, selected)
}

pub(super) fn rgba_color([r, g, b, a]: [u8; 4]) -> gdk::RGBA {
    let unit = |v: u8| v as f32 / 255.0;
    gdk::RGBA::new(unit(r), unit(g), unit(b), unit(a))
}

pub(super) fn rgba_bytes(color: gdk::RGBA) -> [u8; 4] {
    [
        (color.red() * 255.0).round() as u8,
        (color.green() * 255.0).round() as u8,
        (color.blue() * 255.0).round() as u8,
        (color.alpha() * 255.0).round() as u8,
    ]
}

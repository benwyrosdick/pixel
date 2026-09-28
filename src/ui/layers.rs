//! The layer list with thumbnails, the blend mode, opacity slider, position
//! and size fields, and layer buttons.

use super::canvas::upload;
use super::model::Session;
use super::shell::adjustment_label;
use super::shell::Shell;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use pixel::document::{BlendMode, Command, Layer, LayerKind, PixelRect, MAX_EDGE};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

pub struct LayersPanel {
    pub root: gtk::Box,
    list: gtk::ListBox,
    opacity: gtk::Scale,
    blend: gtk::DropDown,
    geometry: Geometry,
    /// Thumbnails by layer id, with the pixel buffer each was made from.
    thumbnails: RefCell<HashMap<u64, Thumbnail>>,
    /// The layer each row shows, top row first. Folded groups hide rows, so
    /// rows and layers don't line up one to one.
    rows: Rc<RefCell<Vec<usize>>>,
    updating: Rc<Cell<bool>>,
    dragging: Rc<Cell<bool>>,
    opacity_before: Rc<Cell<f32>>,
}

impl LayersPanel {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        // Child scales want to expand. Pin that here so the panel stays a
        // fixed column until the splitter is dragged.
        root.set_hexpand(false);
        root.set_hexpand_set(true);
        root.set_vexpand(true);
        root.set_width_request(200);
        root.set_margin_top(8);
        root.set_margin_bottom(8);
        root.set_margin_start(8);
        root.set_margin_end(8);
        root.add_css_class("pixel-panel");

        let title = gtk::Label::new(Some("Layers"));
        title.set_xalign(0.0);
        root.append(&title);

        let list = gtk::ListBox::new();
        // Ctrl-click and Shift-click select several rows.
        list.set_selection_mode(gtk::SelectionMode::Multiple);
        list.set_vexpand(true);
        let scroller = gtk::ScrolledWindow::new();
        scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);
        scroller.set_hexpand(true);
        scroller.set_propagate_natural_width(false);
        scroller.set_child(Some(&list));
        root.append(&scroller);

        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let add = gtk::Button::from_icon_name("list-add-symbolic");
        add.set_tooltip_text(Some("Add layer"));
        let duplicate = gtk::Button::from_icon_name("edit-copy-symbolic");
        duplicate.set_tooltip_text(Some("Duplicate layer"));
        let delete = gtk::Button::from_icon_name("user-trash-symbolic");
        delete.set_tooltip_text(Some("Delete layer"));
        let up = gtk::Button::from_icon_name("go-up-symbolic");
        up.set_tooltip_text(Some("Raise layer"));
        let down = gtk::Button::from_icon_name("go-down-symbolic");
        down.set_tooltip_text(Some("Lower layer"));
        for button in [&add, &duplicate, &delete, &up, &down] {
            buttons.append(button);
        }
        root.append(&buttons);

        let opacity = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
        opacity.set_hexpand(true);
        opacity.set_draw_value(true);
        root.append(&gtk::Label::builder().label("Opacity").xalign(0.0).build());
        root.append(&opacity);

        let labels: Vec<&str> = BlendMode::ALL
            .iter()
            .map(|&mode| blend_label(mode))
            .collect();
        let blend = gtk::DropDown::from_strings(&labels);
        blend.set_hexpand(true);
        blend.set_tooltip_text(Some("How the layer mixes with the layers under it"));
        let blend_row = gtk::Grid::new();
        blend_row.set_column_spacing(8);
        blend_row.attach(&gtk::Label::new(Some("Blend")), 0, 0, 1, 1);
        blend_row.attach(&blend, 1, 0, 1, 1);
        root.append(&blend_row);

        let geometry = Geometry::new();
        root.append(
            &gtk::Label::builder()
                .label("Position and size")
                .xalign(0.0)
                .build(),
        );
        root.append(&geometry.grid);

        let panel = Self {
            root,
            list,
            opacity,
            blend,
            geometry,
            thumbnails: RefCell::new(HashMap::new()),
            rows: Rc::new(RefCell::new(Vec::new())),
            updating: Rc::new(Cell::new(false)),
            dragging: Rc::new(Cell::new(false)),
            opacity_before: Rc::new(Cell::new(1.0)),
        };
        let _ = (add, duplicate, delete, up, down);
        panel
    }

    pub fn buttons(&self) -> LayerButtons {
        let mut found = Vec::new();
        let mut child = self.root.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Some(row) = widget.downcast_ref::<gtk::Box>() {
                let mut inner = row.first_child();
                while let Some(button) = inner {
                    inner = button.next_sibling();
                    if button.is::<gtk::Button>() {
                        found.push(button.downcast::<gtk::Button>().unwrap());
                    }
                }
            }
        }
        LayerButtons {
            add: found[0].clone(),
            duplicate: found[1].clone(),
            delete: found[2].clone(),
            up: found[3].clone(),
            down: found[4].clone(),
        }
    }

    pub fn connect(&self, shell: &Rc<Shell>) {
        let shell_select = shell.clone();
        let updating = self.updating.clone();
        let rows = self.rows.clone();
        self.list.connect_selected_rows_changed(move |list| {
            if updating.get() {
                return;
            }
            let rows = rows.borrow();
            let mut indices: Vec<usize> = list
                .selected_rows()
                .iter()
                .filter_map(|row| usize::try_from(row.index()).ok())
                .filter_map(|row| rows.get(row).copied())
                .collect();
            indices.sort_unstable();
            // Applying the selection rebuilds these rows, so wait until the
            // list has finished its own update.
            let shell = shell_select.clone();
            glib::idle_add_local_once(move || shell.set_selection(indices));
        });

        let dragging = self.dragging.clone();
        let before = self.opacity_before.clone();
        let shell_press = shell.clone();
        let gesture = gtk::GestureClick::new();
        gesture.set_button(gdk::BUTTON_PRIMARY);
        gesture.connect_pressed(move |_, _, _, _| {
            dragging.set(true);
            if let Some(opacity) = shell_press.active_opacity() {
                before.set(opacity);
            }
        });
        let dragging = self.dragging.clone();
        let shell_release = shell.clone();
        let scale = self.opacity.clone();
        gesture.connect_released(move |_, _, _, _| {
            dragging.set(false);
            shell_release.commit_opacity(scale.value() as f32 / 100.0);
        });
        self.opacity.add_controller(gesture);

        let dragging = self.dragging.clone();
        let updating = self.updating.clone();
        let shell_change = shell.clone();
        let scale = self.opacity.clone();
        self.opacity.connect_value_changed(move |_| {
            if updating.get() {
                return;
            }
            let opacity = scale.value() as f32 / 100.0;
            if dragging.get() {
                shell_change.preview_opacity(opacity);
            } else {
                shell_change.commit_opacity(opacity);
            }
        });

        let updating = self.updating.clone();
        let shell_blend = shell.clone();
        self.blend.connect_selected_notify(move |dropdown| {
            if updating.get() {
                return;
            }
            if let Some(&blend) = BlendMode::ALL.get(dropdown.selected() as usize) {
                shell_blend.set_active_blend(blend);
            }
        });

        self.geometry.connect(shell, &self.updating);
    }

    pub fn sync(&self, shell: &Rc<Shell>, session: &Session) {
        self.updating.set(true);
        let focused = self.focused_row();
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        let doc = session.editor.document();
        self.thumbnails
            .borrow_mut()
            .retain(|id, _| doc.layers().iter().any(|layer| layer.id == *id));
        let mut rows = Vec::new();
        for (index, layer) in doc.layers().iter().enumerate().rev() {
            if doc.hidden_in_panel(index) {
                continue;
            }
            let thumbnail = match layer.kind {
                LayerKind::Group { .. } => None,
                _ => Some(self.thumbnail(layer)),
            };
            let depth = doc.depth(index);
            self.list
                .append(&layer_row(shell, index, depth, layer, thumbnail.as_ref()));
            rows.push(index);
        }
        // Removing the focused row makes GTK move focus back into the list
        // later, and a row that gains focus that way gets selected. That
        // selection would arrive after this sync and undo the document's.
        // Focusing the new row now, while updates are ignored, prevents it.
        if let Some(visual) = focused {
            let last = self.list.observe_children().n_items().saturating_sub(1) as i32;
            if let Some(row) = self.list.row_at_index(visual.min(last)) {
                row.grab_focus();
            }
        }
        self.list.unselect_all();
        for index in doc.selected_indices() {
            let Some(visual) = rows.iter().position(|&row| row == index) else {
                continue;
            };
            if let Some(row) = self.list.row_at_index(visual as i32) {
                self.list.select_row(Some(&row));
            }
        }
        *self.rows.borrow_mut() = rows;
        let active = doc.active_layer();
        if !self.dragging.get() {
            let opacity = active.map_or(1.0, |layer| layer.opacity) * 100.0;
            self.opacity.set_value(opacity as f64);
        }
        self.opacity
            .set_sensitive(active.is_some_and(|layer| !layer.locked));
        if let Some(layer) = active {
            let position = BlendMode::ALL.iter().position(|&mode| mode == layer.blend);
            self.blend.set_selected(position.unwrap_or(0) as u32);
        }
        self.blend
            .set_sensitive(active.is_some_and(|layer| !layer.locked));
        self.geometry
            .show(active.filter(|layer| !matches!(layer.kind, LayerKind::Group { .. })));
        let buttons = self.buttons();
        for button in [&buttons.duplicate, &buttons.up, &buttons.down] {
            button.set_sensitive(active.is_some());
        }
        buttons
            .delete
            .set_sensitive(!doc.selected_indices().is_empty());
        self.updating.set(false);
    }

    /// The layer's thumbnail, remade only when its pixels change. Every edit
    /// that changes pixels puts them in a new buffer, so a buffer's address
    /// and size tell whether a cached thumbnail is still current.
    fn thumbnail(&self, layer: &Layer) -> gdk::Texture {
        let key = (
            layer.pixels.as_raw().as_ptr() as usize,
            layer.width(),
            layer.height(),
        );
        let mut cache = self.thumbnails.borrow_mut();
        if let Some(cached) = cache.get(&layer.id).filter(|cached| cached.key == key) {
            return cached.texture.clone();
        }
        let texture = upload(&thumbnail_image(&layer.pixels)).upcast::<gdk::Texture>();
        cache.insert(
            layer.id,
            Thumbnail {
                key,
                texture: texture.clone(),
            },
        );
        texture
    }

    /// The position of the row that has keyboard focus, or holds the widget
    /// that does.
    fn focused_row(&self) -> Option<i32> {
        let focus = self.list.root()?.focus()?;
        let mut index = 0;
        while let Some(row) = self.list.row_at_index(index) {
            if focus == *row.upcast_ref::<gtk::Widget>() || focus.is_ancestor(&row) {
                return Some(index);
            }
            index += 1;
        }
        None
    }

    pub fn clear(&self) {
        self.updating.set(true);
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        self.updating.set(false);
    }
}

/// Unlocked rows show a faint open padlock so the toggle is discoverable
/// without competing with the name.
fn show_lock(button: &gtk::ToggleButton, locked: bool) {
    if locked {
        button.set_icon_name("changes-prevent-symbolic");
        button.set_tooltip_text(Some("Unlock layer"));
        button.set_opacity(1.0);
    } else {
        button.set_icon_name("changes-allow-symbolic");
        button.set_tooltip_text(Some("Lock layer"));
        button.set_opacity(0.45);
    }
}

/// X, Y, width, and height fields for the one selected layer, with a toggle
/// that keeps the aspect ratio while the size changes.
struct Geometry {
    grid: gtk::Grid,
    x: gtk::SpinButton,
    y: gtk::SpinButton,
    width: gtk::SpinButton,
    height: gtk::SpinButton,
    keep_aspect: gtk::ToggleButton,
}

impl Geometry {
    fn new() -> Self {
        let reach = MAX_EDGE as f64 * 4.0;
        let field = |min: f64, max: f64, tooltip: &str| {
            let spin = gtk::SpinButton::with_range(min, max, 1.0);
            spin.set_digits(0);
            spin.set_numeric(true);
            spin.set_width_chars(5);
            spin.set_hexpand(true);
            spin.set_tooltip_text(Some(tooltip));
            spin
        };
        let x = field(-reach, reach, "Left edge, in pixels");
        let y = field(-reach, reach, "Top edge, in pixels");
        let width = field(1.0, MAX_EDGE as f64, "Width, in pixels");
        let height = field(1.0, MAX_EDGE as f64, "Height, in pixels");
        let keep_aspect = gtk::ToggleButton::new();
        keep_aspect.set_icon_name("insert-link-symbolic");
        keep_aspect.set_tooltip_text(Some("Keep the aspect ratio"));
        keep_aspect.set_valign(gtk::Align::Center);

        let grid = gtk::Grid::new();
        grid.set_row_spacing(4);
        grid.set_column_spacing(6);
        let label = |text: &str| gtk::Label::builder().label(text).xalign(0.0).build();
        grid.attach(&label("X"), 0, 0, 1, 1);
        grid.attach(&x, 1, 0, 1, 1);
        grid.attach(&label("Y"), 2, 0, 1, 1);
        grid.attach(&y, 3, 0, 1, 1);
        grid.attach(&label("W"), 0, 1, 1, 1);
        grid.attach(&width, 1, 1, 1, 1);
        grid.attach(&label("H"), 2, 1, 1, 1);
        grid.attach(&height, 3, 1, 1, 1);
        grid.attach(&keep_aspect, 4, 1, 1, 1);
        Self {
            grid,
            x,
            y,
            width,
            height,
            keep_aspect,
        }
    }

    fn connect(&self, shell: &Rc<Shell>, updating: &Rc<Cell<bool>>) {
        let fields = [&self.x, &self.y, &self.width, &self.height];
        for (field, sized) in fields
            .into_iter()
            .zip([None, None, Some(true), Some(false)])
        {
            let shell = shell.clone();
            let updating = updating.clone();
            let (x, y, width, height) = (
                self.x.clone(),
                self.y.clone(),
                self.width.clone(),
                self.height.clone(),
            );
            let keep_aspect = self.keep_aspect.clone();
            field.connect_value_changed(move |_| {
                if updating.get() {
                    return;
                }
                let Some(current) = shell.active_geometry() else {
                    return;
                };
                // Carry the size change to the other side, in proportion.
                if let (Some(changed_width), true) = (sized, keep_aspect.is_active()) {
                    let ratio = current.height as f64 / current.width as f64;
                    updating.set(true);
                    if changed_width {
                        height.set_value((width.value() * ratio).round().max(1.0));
                    } else {
                        width.set_value((height.value() / ratio).round().max(1.0));
                    }
                    updating.set(false);
                }
                shell.set_active_geometry(PixelRect {
                    x: x.value() as i32,
                    y: y.value() as i32,
                    width: width.value() as u32,
                    height: height.value() as u32,
                });
            });
        }
    }

    /// Show the selected layer's box. The fields grey out unless exactly one
    /// unlocked layer is selected.
    fn show(&self, active: Option<&Layer>) {
        if let Some(layer) = active {
            self.x.set_value(layer.x as f64);
            self.y.set_value(layer.y as f64);
            self.width.set_value(layer.width() as f64);
            self.height.set_value(layer.height() as f64);
        }
        self.grid
            .set_sensitive(active.is_some_and(|layer| !layer.locked));
    }
}

pub struct LayerButtons {
    pub add: gtk::Button,
    pub duplicate: gtk::Button,
    pub delete: gtk::Button,
    pub up: gtk::Button,
    pub down: gtk::Button,
}

struct Thumbnail {
    key: (usize, u32, u32),
    texture: gdk::Texture,
}

/// How big a thumbnail draws, in screen pixels. It is rendered at twice that
/// so it stays sharp on high-density screens.
const THUMBNAIL_SIZE: i32 = 32;

/// The layer's pixels fitted into a thumbnail, over a checkerboard so
/// transparent areas read as transparent.
fn thumbnail_image(pixels: &image::RgbaImage) -> image::RgbaImage {
    let size = THUMBNAIL_SIZE as u32 * 2;
    let (w, h) = pixels.dimensions();
    let scale = size as f64 / w.max(h) as f64;
    let tw = ((w as f64 * scale).round() as u32).clamp(1, size);
    let th = ((h as f64 * scale).round() as u32).clamp(1, size);
    let mut small = if scale < 1.0 {
        image::imageops::thumbnail(pixels, tw, th)
    } else {
        image::imageops::resize(pixels, tw, th, image::imageops::FilterType::Nearest)
    };
    for (x, y, pixel) in small.enumerate_pixels_mut() {
        let square = if (x / 8 + y / 8) % 2 == 0 {
            204.0
        } else {
            153.0
        };
        let alpha = pixel[3] as f32 / 255.0;
        for channel in 0..3 {
            pixel[channel] = (pixel[channel] as f32 * alpha + square * (1.0 - alpha)).round() as u8;
        }
        pixel[3] = 255;
    }
    small
}

fn blend_label(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Normal => "Normal",
        BlendMode::Multiply => "Multiply",
        BlendMode::Screen => "Screen",
        BlendMode::Overlay => "Overlay",
        BlendMode::Darken => "Darken",
        BlendMode::Lighten => "Lighten",
        BlendMode::ColorDodge => "Color Dodge",
        BlendMode::ColorBurn => "Color Burn",
        BlendMode::HardLight => "Hard Light",
        BlendMode::SoftLight => "Soft Light",
        BlendMode::Difference => "Difference",
        BlendMode::Exclusion => "Exclusion",
    }
}

/// How far each level of grouping indents a row.
const INDENT: i32 = 16;

/// A layer's row. A group's row has a fold arrow and a folder in place of a
/// thumbnail, and rows inside groups are indented by `depth`.
fn layer_row(
    shell: &Rc<Shell>,
    index: usize,
    depth: usize,
    layer: &Layer,
    thumbnail: Option<&gdk::Texture>,
) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("pixel-layer");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.set_margin_top(4);
    content.set_margin_bottom(4);
    content.set_margin_start(4 + INDENT * depth as i32);
    content.set_margin_end(4);
    let group = match layer.kind {
        LayerKind::Group { collapsed } => Some(collapsed),
        _ => None,
    };
    if let Some(collapsed) = group {
        let fold = gtk::Button::from_icon_name(if collapsed {
            "pan-end-symbolic"
        } else {
            "pan-down-symbolic"
        });
        fold.add_css_class("flat");
        fold.set_valign(gtk::Align::Center);
        fold.set_tooltip_text(Some(if collapsed {
            "Show contents"
        } else {
            "Hide contents"
        }));
        let shell = shell.clone();
        fold.connect_clicked(move |_| {
            let shell = shell.clone();
            glib::idle_add_local_once(move || shell.set_collapsed(index, !collapsed));
        });
        content.append(&fold);
    }

    let eye = gtk::ToggleButton::new();
    eye.set_icon_name(if layer.visible {
        "view-reveal-symbolic"
    } else {
        "view-conceal-symbolic"
    });
    eye.set_active(layer.visible);
    eye.set_tooltip_text(Some("Show layer"));
    let shell_eye = shell.clone();
    eye.connect_toggled(move |button| {
        let visible = button.is_active();
        button.set_icon_name(if visible {
            "view-reveal-symbolic"
        } else {
            "view-conceal-symbolic"
        });
        let shell = shell_eye.clone();
        glib::idle_add_local_once(move || {
            shell.edit(Command::SetVisibility { index, visible });
        });
    });

    let lock = gtk::ToggleButton::new();
    lock.set_active(layer.locked);
    show_lock(&lock, layer.locked);
    let shell_lock = shell.clone();
    lock.connect_toggled(move |button| {
        let locked = button.is_active();
        show_lock(button, locked);
        let shell = shell_lock.clone();
        glib::idle_add_local_once(move || {
            shell.edit(Command::SetLocked { index, locked });
        });
    });

    // A plain label, not an entry. A click selects the row. Double-click
    // swaps in the entry to rename.
    let name_label = gtk::Label::new(Some(&layer.name));
    name_label.set_xalign(0.0);
    name_label.set_hexpand(true);
    name_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name_label.set_can_target(false);
    row.set_tooltip_text(Some("Double-click the name to rename"));

    let entry = gtk::Entry::new();
    entry.set_hexpand(true);
    entry.set_has_frame(false);
    entry.set_text(&layer.name);

    let name_box = gtk::Stack::new();
    name_box.set_hexpand(true);
    name_box.add_named(&name_label, Some("label"));
    name_box.add_named(&entry, Some("edit"));

    let editing = Rc::new(Cell::new(false));
    let original = layer.name.clone();
    let end_edit: Rc<dyn Fn(bool)> = Rc::new({
        let entry = entry.clone();
        let name_box = name_box.clone();
        let editing = editing.clone();
        let shell = shell.clone();
        let original = original.clone();
        move |save: bool| {
            if !editing.get() {
                return;
            }
            editing.set(false);
            let name = entry.text().to_string();
            name_box.set_visible_child_name("label");
            if save && name != original {
                let shell = shell.clone();
                glib::idle_add_local_once(move || {
                    shell.edit(Command::Rename { index, name });
                });
            }
        }
    });

    let begin_edit = {
        let entry = entry.clone();
        let name_box = name_box.clone();
        let editing = editing.clone();
        let original = original.clone();
        move || {
            if editing.get() {
                return;
            }
            editing.set(true);
            entry.set_text(&original);
            name_box.set_visible_child_name("edit");
            entry.grab_focus();
            entry.select_region(0, -1);
        }
    };

    let end_on_activate = Rc::clone(&end_edit);
    entry.connect_activate(move |_| end_on_activate(true));

    let end_on_leave = Rc::clone(&end_edit);
    let focus = gtk::EventControllerFocus::new();
    focus.connect_leave(move |_| end_on_leave(true));
    entry.add_controller(focus);

    let end_on_escape = Rc::clone(&end_edit);
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            end_on_escape(false);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    entry.add_controller(keys);

    let row_for_hit = row.clone();
    let name_for_hit = name_box.clone();
    let rename_click = gtk::GestureClick::new();
    rename_click.set_button(gdk::BUTTON_PRIMARY);
    rename_click.connect_pressed(move |_, n_press, x, _| {
        let on_name = name_for_hit
            .compute_bounds(&row_for_hit)
            .is_some_and(|bounds| x >= f64::from(bounds.x()));
        if n_press == 2 && on_name {
            begin_edit();
        }
    });
    row.add_controller(rename_click);

    // Right-click opens the layer's menu, with its adjustments.
    let menu_click = gtk::GestureClick::new();
    menu_click.set_button(gdk::BUTTON_SECONDARY);
    let shell_menu = shell.clone();
    let row_for_menu = row.clone();
    menu_click.connect_pressed(move |gesture, _, x, y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        shell_menu.show_layer_menu(index, row_for_menu.upcast_ref(), x, y);
    });
    row.add_controller(menu_click);

    let picture: gtk::Widget = match thumbnail {
        Some(thumbnail) => {
            let picture = gtk::Picture::for_paintable(thumbnail);
            picture.set_content_fit(gtk::ContentFit::Contain);
            picture.set_can_shrink(true);
            picture.upcast()
        }
        None => {
            let folder = gtk::Image::from_icon_name("folder-symbolic");
            folder.set_pixel_size(THUMBNAIL_SIZE * 3 / 4);
            folder.upcast()
        }
    };
    picture.set_size_request(THUMBNAIL_SIZE, THUMBNAIL_SIZE);
    picture.set_can_target(false);

    // Rows are as tall as the thumbnail. Keep the toggles their own size.
    eye.set_valign(gtk::Align::Center);
    lock.set_valign(gtk::Align::Center);
    content.append(&eye);
    content.append(&lock);
    content.append(&picture);
    content.append(&name_box);
    if !layer.adjustments.is_empty() {
        let badge = gtk::Image::from_icon_name("color-select-symbolic");
        badge.set_valign(gtk::Align::Center);
        badge.add_css_class("dim-label");
        let names: Vec<String> = layer.adjustments.iter().map(adjustment_label).collect();
        badge.set_tooltip_text(Some(&format!(
            "Adjusted: {}. Right-click to remove.",
            names.join(", ")
        )));
        content.append(&badge);
    }
    row.set_child(Some(&content));

    let source = gtk::DragSource::new();
    source.set_actions(gdk::DragAction::MOVE);
    source.connect_prepare(move |_, _, _| {
        let value = (index as i32).to_value();
        Some(gdk::ContentProvider::for_value(&value))
    });
    row.add_controller(source);

    let drop = gtk::DropTarget::new(i32::static_type(), gdk::DragAction::MOVE);
    let shell_drop = shell.clone();
    drop.connect_drop(move |_, value, _, _| {
        let Ok(from) = value.get::<i32>() else {
            return false;
        };
        let shell = shell_drop.clone();
        let from = from as usize;
        // Dropping onto a group's row puts the layer inside it.
        glib::idle_add_local_once(move || match group {
            Some(_) if from != index => shell.edit(Command::MoveIntoGroup {
                index: from,
                group: index,
            }),
            _ => shell.edit(Command::Reorder { from, to: index }),
        });
        true
    });
    row.add_controller(drop);
    row
}

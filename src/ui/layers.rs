//! The layer list, opacity slider, and layer buttons.

use super::model::Session;
use super::shell::Shell;
use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use pixel::document::{Command, Layer};
use std::cell::Cell;
use std::rc::Rc;

pub struct LayersPanel {
    pub root: gtk::Box,
    list: gtk::ListBox,
    opacity: gtk::Scale,
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

        let panel = Self {
            root,
            list,
            opacity,
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
        self.list.connect_selected_rows_changed(move |list| {
            if updating.get() {
                return;
            }
            let len = shell_select.layer_count();
            // Rows run top to bottom, layers bottom to top.
            let indices: Vec<usize> = list
                .selected_rows()
                .iter()
                .filter_map(|row| usize::try_from(row.index()).ok())
                .filter(|&visual| visual < len)
                .map(|visual| len - 1 - visual)
                .collect();
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
    }

    pub fn sync(&self, shell: &Rc<Shell>, session: &Session) {
        self.updating.set(true);
        while let Some(row) = self.list.row_at_index(0) {
            self.list.remove(&row);
        }
        let doc = session.editor.document();
        for (index, layer) in doc.layers().iter().enumerate().rev() {
            self.list.append(&layer_row(shell, index, layer));
        }
        self.list.unselect_all();
        for index in doc.selected_indices() {
            let visual = (doc.layers().len() - 1 - index) as i32;
            if let Some(row) = self.list.row_at_index(visual) {
                self.list.select_row(Some(&row));
            }
        }
        let active = doc.active_layer();
        if !self.dragging.get() {
            let opacity = active.map_or(1.0, |layer| layer.opacity) * 100.0;
            self.opacity.set_value(opacity as f64);
        }
        self.opacity
            .set_sensitive(active.is_some_and(|layer| !layer.locked));
        let buttons = self.buttons();
        for button in [&buttons.duplicate, &buttons.up, &buttons.down] {
            button.set_sensitive(active.is_some());
        }
        buttons
            .delete
            .set_sensitive(!doc.selected_indices().is_empty());
        self.updating.set(false);
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

pub struct LayerButtons {
    pub add: gtk::Button,
    pub duplicate: gtk::Button,
    pub delete: gtk::Button,
    pub up: gtk::Button,
    pub down: gtk::Button,
}

fn layer_row(shell: &Rc<Shell>, index: usize, layer: &Layer) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.add_css_class("pixel-layer");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.set_margin_top(4);
    content.set_margin_bottom(4);
    content.set_margin_start(4);
    content.set_margin_end(4);

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

    content.append(&eye);
    content.append(&lock);
    content.append(&name_box);
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
        glib::idle_add_local_once(move || {
            shell.edit(Command::Reorder {
                from: from as usize,
                to: index,
            });
        });
        true
    });
    row.add_controller(drop);
    row
}

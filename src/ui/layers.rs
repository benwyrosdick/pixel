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
        list.set_selection_mode(gtk::SelectionMode::Single);
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
        self.list.connect_row_selected(move |_, row| {
            if updating.get() {
                return;
            }
            let Some(row) = row else {
                return;
            };
            let visual = row.index();
            if visual < 0 {
                return;
            }
            let len = shell_select.layer_count();
            if visual as usize >= len {
                return;
            }
            shell_select.set_active(len - 1 - visual as usize);
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
        let visual = doc.layers().len() as i32 - 1 - doc.active_index() as i32;
        if let Some(row) = self.list.row_at_index(visual) {
            self.list.select_row(Some(&row));
        }
        if !self.dragging.get() {
            let opacity = doc.active_layer().opacity * 100.0;
            self.opacity.set_value(opacity as f64);
        }
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

    let name = gtk::EditableLabel::new(&layer.name);
    name.set_hexpand(true);
    let shell_name = shell.clone();
    name.connect_editing_notify(move |label| {
        if label.is_editing() {
            return;
        }
        let name = label.text().to_string();
        let shell = shell_name.clone();
        glib::idle_add_local_once(move || {
            shell.edit(Command::Rename { index, name });
        });
    });

    content.append(&eye);
    content.append(&name);
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

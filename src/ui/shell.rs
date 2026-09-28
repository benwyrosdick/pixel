//! The editor window. Document changes go through [`Shell::edit`].

use super::canvas::{Canvas, CanvasInput};
use super::dialogs;
use super::layers::LayersPanel;
use super::model::{
    document_title, fit_view, jpeg_needs_white, widget_to_doc, CropDraft, Model, Preview, Tool,
};
use super::theme::{self, ThemeColors};
use gtk::gdk;
use gtk::{gio, glib, prelude::*};
use image::ImageReader;
use libadwaita::prelude::*;
use pixel::document::{
    export, open_image, open_project, save_project, Axis, Command, Document, Editor, ExportFormat,
    NewCanvas, QuarterTurn,
};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub struct Shell {
    pub model: Rc<RefCell<Model>>,
    pub window: libadwaita::ApplicationWindow,
    toasts: libadwaita::ToastOverlay,
    stack: gtk::Stack,
    title: gtk::Label,
    tool_options: gtk::Box,
    status_size: gtk::Label,
    status_zoom: gtk::Label,
    status_cursor: gtk::Label,
    pub canvas: Canvas,
    layers: LayersPanel,
    move_tool: gtk::ToggleButton,
    crop_tool: gtk::ToggleButton,
    undo_btn: gtk::Button,
    redo_btn: gtk::Button,
    export_btn: gtk::Button,
    suppress: Cell<bool>,
    force_close: Cell<bool>,
    drag: RefCell<Option<Drag>>,
    crop_label: RefCell<Option<gtk::Label>>,
    theme_monitor: RefCell<Option<gio::FileMonitor>>,
}

#[derive(Clone)]
struct Drag {
    kind: DragKind,
    origin_x: f64,
    origin_y: f64,
}

#[derive(Clone)]
enum DragKind {
    Pan { pan_x: f64, pan_y: f64 },
    Move { index: usize, x: i32, y: i32 },
    Crop { ax: f64, ay: f64 },
}

enum Next {
    Quit,
    Run(Box<dyn FnOnce(&Rc<Shell>)>),
}

thread_local! {
    static OPEN: RefCell<Option<Rc<Shell>>> = const { RefCell::new(None) };
}

pub fn present(app: &libadwaita::Application, open: Option<PathBuf>) {
    let shell = OPEN.with(|slot| {
        if let Some(shell) = slot.borrow().clone() {
            return shell;
        }
        let shell = Shell::build(app);
        *slot.borrow_mut() = Some(shell.clone());
        install_theme(&shell);
        shell
    });
    shell.window.present();
    if let Some(path) = open {
        shell.open_path(&path);
    }
}

impl Shell {
    fn build(app: &libadwaita::Application) -> Rc<Self> {
        let colors = ThemeColors::load();
        let model = Rc::new(RefCell::new(Model {
            session: None,
            accent: colors.accent_rgb,
            background: colors.background_rgb,
        }));

        let canvas = Canvas::new();
        canvas.set_model(model.clone());
        let layers = LayersPanel::new();

        let move_tool = tool_button("Move");
        let crop_tool = tool_button("Crop");
        crop_tool.set_group(Some(&move_tool));
        move_tool.set_active(true);
        let tools = gtk::Box::new(gtk::Orientation::Vertical, 0);
        tools.add_css_class("pixel-panel");
        tools.set_width_request(76);
        tools.append(&move_tool);
        tools.append(&crop_tool);

        let editor = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        editor.append(&tools);
        editor.append(&canvas);
        editor.append(&layers.root);

        let (welcome, new_canvas, open_image, open_project) = welcome_page();
        let stack = gtk::Stack::new();
        stack.add_named(&welcome, Some("welcome"));
        stack.add_named(&editor, Some("editor"));

        let tool_options = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        tool_options.add_css_class("pixel-toolbar");
        tool_options.set_margin_top(4);
        tool_options.set_margin_bottom(4);
        tool_options.set_margin_start(8);
        tool_options.set_margin_end(8);

        let title = gtk::Label::new(Some("Pixel"));
        let undo_btn = gtk::Button::from_icon_name("edit-undo-symbolic");
        undo_btn.set_tooltip_text(Some("Undo"));
        let redo_btn = gtk::Button::from_icon_name("edit-redo-symbolic");
        redo_btn.set_tooltip_text(Some("Redo"));
        let export_btn = gtk::Button::with_label("Export");
        let menu_btn = gtk::MenuButton::new();
        menu_btn.set_icon_name("open-menu-symbolic");
        menu_btn.set_menu_model(Some(&app_menu()));

        let header = libadwaita::HeaderBar::new();
        header.pack_start(&undo_btn);
        header.pack_start(&redo_btn);
        header.pack_end(&menu_btn);
        header.pack_end(&export_btn);
        header.set_title_widget(Some(&title));

        let top = gtk::Box::new(gtk::Orientation::Vertical, 0);
        top.append(&header);
        top.append(&tool_options);

        let status_size = gtk::Label::new(None);
        let status_cursor = gtk::Label::new(None);
        let status_zoom = gtk::Label::new(None);
        let fit = gtk::Button::with_label("Fit");
        let actual = gtk::Button::with_label("100%");
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        let status = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        status.add_css_class("pixel-status");
        status.set_margin_top(4);
        status.set_margin_bottom(4);
        status.set_margin_start(8);
        status.set_margin_end(8);
        status.append(&status_size);
        status.append(&status_cursor);
        status.append(&spacer);
        status.append(&status_zoom);
        status.append(&fit);
        status.append(&actual);

        let toolbar = libadwaita::ToolbarView::new();
        toolbar.add_top_bar(&top);
        toolbar.add_bottom_bar(&status);
        toolbar.set_content(Some(&stack));

        let toasts = libadwaita::ToastOverlay::new();
        toasts.set_child(Some(&toolbar));

        let window = libadwaita::ApplicationWindow::new(app);
        window.set_title(Some("Pixel"));
        window.set_default_size(1200, 800);
        window.add_css_class("pixel-root");
        window.set_content(Some(&toasts));

        let shell = Rc::new(Self {
            model,
            window,
            toasts,
            stack,
            title,
            tool_options,
            status_size,
            status_zoom,
            status_cursor,
            canvas,
            layers,
            move_tool,
            crop_tool,
            undo_btn,
            redo_btn,
            export_btn,
            suppress: Cell::new(false),
            force_close: Cell::new(false),
            drag: RefCell::new(None),
            crop_label: RefCell::new(None),
            theme_monitor: RefCell::new(None),
        });
        shell.bind(&new_canvas, &open_image, &open_project, &fit, &actual);
        shell.refresh();
        shell
    }

    fn bind(
        self: &Rc<Self>,
        new_canvas: &gtk::Button,
        open_image: &gtk::Button,
        open_project: &gtk::Button,
        fit: &gtk::Button,
        actual: &gtk::Button,
    ) {
        let shell = self.clone();
        self.canvas
            .set_handler(Rc::new(move |event| shell.on_canvas(event)));

        self.layers.connect(self);
        let buttons = self.layers.buttons();
        let shell = self.clone();
        buttons
            .add
            .connect_clicked(move |_| shell.edit(Command::AddLayer));
        let shell = self.clone();
        buttons.duplicate.connect_clicked(move |_| {
            let index = shell.active_index();
            if let Some(index) = index {
                shell.edit(Command::DuplicateLayer { index });
            }
        });
        let shell = self.clone();
        buttons.delete.connect_clicked(move |_| {
            if let Some(index) = shell.active_index() {
                shell.edit(Command::DeleteLayer { index });
            }
        });
        let shell = self.clone();
        buttons.up.connect_clicked(move |_| shell.reorder_active(1));
        let shell = self.clone();
        buttons
            .down
            .connect_clicked(move |_| shell.reorder_active(-1));

        let shell = self.clone();
        self.move_tool.connect_toggled(move |button| {
            if button.is_active() {
                shell.set_tool(Tool::Move);
            }
        });
        let shell = self.clone();
        self.crop_tool.connect_toggled(move |button| {
            if button.is_active() {
                shell.set_tool(Tool::Crop);
            }
        });

        let shell = self.clone();
        self.undo_btn.connect_clicked(move |_| shell.undo());
        let shell = self.clone();
        self.redo_btn.connect_clicked(move |_| shell.redo());
        let shell = self.clone();
        self.export_btn.connect_clicked(move |_| shell.export());
        let shell = self.clone();
        fit.connect_clicked(move |_| shell.zoom_fit());
        let shell = self.clone();
        actual.connect_clicked(move |_| shell.zoom_actual());

        let shell = self.clone();
        new_canvas.connect_clicked(move |_| shell.new_canvas());
        let shell = self.clone();
        open_image.connect_clicked(move |_| shell.open_image());
        let shell = self.clone();
        open_project.connect_clicked(move |_| shell.open_project());

        self.add_action("new", &["<primary>n"], |shell| shell.new_canvas());
        self.add_action("open", &["<primary>o"], |shell| shell.open_image());
        self.add_action("open-project", &[], |shell| shell.open_project());
        self.add_action("save", &["<primary>s"], |shell| {
            shell.save(|_| {});
        });
        self.add_action("save-as", &["<primary><shift>s"], |shell| {
            shell.save_as(|_| {});
        });
        self.add_action("export", &["<primary><shift>e"], |shell| shell.export());
        self.add_action("close", &["<primary>w"], |shell| shell.close_document());
        self.add_action("quit", &["<primary>q"], |shell| {
            shell.window.close();
        });
        self.add_action("undo", &["<primary>z"], |shell| shell.undo());
        self.add_action("redo", &["<primary><shift>z", "<primary>y"], |shell| {
            shell.redo()
        });
        self.add_action("canvas-size", &[], |shell| shell.canvas_size());
        self.add_action("image-size", &[], |shell| shell.image_size());
        self.add_action("rotate-cw", &[], |shell| {
            shell.edit(Command::RotateCanvas {
                turn: QuarterTurn::Cw,
            });
        });
        self.add_action("rotate-ccw", &[], |shell| {
            shell.edit(Command::RotateCanvas {
                turn: QuarterTurn::Ccw,
            });
        });
        self.add_action("rotate-180", &[], |shell| {
            shell.edit(Command::RotateCanvas {
                turn: QuarterTurn::Half,
            });
        });
        self.add_action("flip-h", &[], |shell| {
            shell.edit(Command::FlipCanvas {
                axis: Axis::Horizontal,
            });
        });
        self.add_action("flip-v", &[], |shell| {
            shell.edit(Command::FlipCanvas {
                axis: Axis::Vertical,
            });
        });
        self.add_action("add-layer", &[], |shell| shell.edit(Command::AddLayer));
        self.add_action("place", &[], |shell| shell.place_image());
        self.add_action("duplicate", &[], |shell| {
            if let Some(index) = shell.active_index() {
                shell.edit(Command::DuplicateLayer { index });
            }
        });
        self.add_action("delete-layer", &[], |shell| {
            if let Some(index) = shell.active_index() {
                shell.edit(Command::DeleteLayer { index });
            }
        });
        self.add_action("rotate-layer", &[], |shell| shell.rotate_layer());
        self.add_action("flip-layer-h", &[], |shell| {
            if let Some(index) = shell.active_index() {
                shell.edit(Command::FlipLayer {
                    index,
                    axis: Axis::Horizontal,
                });
            }
        });
        self.add_action("flip-layer-v", &[], |shell| {
            if let Some(index) = shell.active_index() {
                shell.edit(Command::FlipLayer {
                    index,
                    axis: Axis::Vertical,
                });
            }
        });
        self.add_action("raise", &[], |shell| shell.reorder_active(1));
        self.add_action("lower", &[], |shell| shell.reorder_active(-1));
        self.add_action("fit", &["<primary>0"], |shell| shell.zoom_fit());
        self.add_action("actual", &["<primary>1"], |shell| shell.zoom_actual());
        self.add_action("tool-move", &["v"], |shell| shell.set_tool(Tool::Move));
        self.add_action("tool-crop", &["c"], |shell| shell.set_tool(Tool::Crop));

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Bubble);
        let shell = self.clone();
        keys.connect_key_pressed(move |_, key, _, mods| shell.on_key(key, mods));
        let shell = self.clone();
        keys.connect_key_released(move |_, key, _, _| {
            if key == gdk::Key::space {
                if let Some(session) = shell.model.borrow_mut().session.as_mut() {
                    session.space_down = false;
                }
                shell.refresh_cursor_name();
            }
        });
        self.window.add_controller(keys);

        let shell = self.clone();
        self.window.connect_close_request(move |_| {
            if shell.force_close.get() {
                return glib::Propagation::Proceed;
            }
            let dirty = shell
                .model
                .borrow()
                .session
                .as_ref()
                .is_some_and(|session| session.dirty);
            if !dirty {
                return glib::Propagation::Proceed;
            }
            shell.confirm(Next::Quit);
            glib::Propagation::Stop
        });
    }

    fn add_action(self: &Rc<Self>, name: &str, accels: &[&str], f: impl Fn(&Rc<Shell>) + 'static) {
        let action = gio::SimpleAction::new(name, None);
        let shell = self.clone();
        action.connect_activate(move |_, _| f(&shell));
        self.window.add_action(&action);
        if let Some(app) = self.window.application() {
            app.set_accels_for_action(&format!("win.{name}"), accels);
        }
    }

    pub fn edit(self: &Rc<Self>, command: Command) {
        let outcome: Result<(), String> = {
            let mut model = self.model.borrow_mut();
            let Some(session) = model.session.as_mut() else {
                return;
            };
            match session.editor.apply(command) {
                Ok(changed) => {
                    if changed {
                        session.dirty = true;
                    }
                    session.preview = None;
                    session.visual = session.visual.wrapping_add(1);
                    Ok(())
                }
                Err(err) => Err(err.to_string()),
            }
        };
        match outcome {
            Ok(()) => self.refresh(),
            Err(message) => self.toast(&message),
        }
    }

    pub fn set_active(self: &Rc<Self>, index: usize) {
        let changed = {
            let mut model = self.model.borrow_mut();
            let Some(session) = model.session.as_mut() else {
                return;
            };
            if session.editor.document().active_index() == index {
                return;
            }
            session.editor.set_active(index).is_ok()
        };
        if changed {
            self.refresh();
        }
    }

    pub fn layer_count(&self) -> usize {
        self.model
            .borrow()
            .session
            .as_ref()
            .map(|session| session.editor.document().layers().len())
            .unwrap_or(0)
    }

    pub fn active_index(&self) -> Option<usize> {
        self.model
            .borrow()
            .session
            .as_ref()
            .map(|session| session.editor.document().active_index())
    }

    pub fn active_opacity(&self) -> Option<f32> {
        self.model
            .borrow()
            .session
            .as_ref()
            .map(|session| session.editor.document().active_layer().opacity)
    }

    pub fn preview_opacity(&self, opacity: f32) {
        let mut model = self.model.borrow_mut();
        let Some(session) = model.session.as_mut() else {
            return;
        };
        let index = session.editor.document().active_index();
        session.preview = Some(Preview::Opacity { index, opacity });
        session.visual = session.visual.wrapping_add(1);
        drop(model);
        self.canvas.queue_draw();
    }

    pub fn commit_opacity(self: &Rc<Self>, opacity: f32) {
        let Some(index) = self.active_index() else {
            return;
        };
        {
            let mut model = self.model.borrow_mut();
            if let Some(session) = model.session.as_mut() {
                session.preview = None;
            }
        }
        self.edit(Command::SetOpacity { index, opacity });
    }

    fn refresh(self: &Rc<Self>) {
        self.suppress.set(true);
        let model = self.model.borrow();
        let has_document = model.session.is_some();
        self.stack
            .set_visible_child_name(if has_document { "editor" } else { "welcome" });
        self.tool_options.set_visible(has_document);
        let title = document_title(model.session.as_ref());
        self.title.set_label(&title);
        self.window.set_title(Some(&title));
        if let Some(session) = model.session.as_ref() {
            let doc = session.editor.document();
            self.status_size
                .set_label(&format!("{} × {} px", doc.width, doc.height));
            self.status_zoom
                .set_label(&format!("{:.0}%", session.zoom * 100.0));
            self.status_cursor.set_label(&cursor_text(session.cursor));
            self.move_tool
                .set_active(matches!(session.tool, Tool::Move));
            self.crop_tool
                .set_active(matches!(session.tool, Tool::Crop));
            self.undo_btn.set_sensitive(session.editor.can_undo());
            self.redo_btn.set_sensitive(session.editor.can_redo());
            self.export_btn.set_sensitive(true);
            self.enable("undo", session.editor.can_undo());
            self.enable("redo", session.editor.can_redo());
            self.enable_document_actions(true);
            drop(model);
            self.fill_tool_options();
            let model = self.model.borrow();
            if let Some(session) = model.session.as_ref() {
                self.layers.sync(self, session);
            }
            drop(model);
            self.refresh_cursor_name();
        } else {
            self.status_size.set_label("");
            self.status_zoom.set_label("");
            self.status_cursor.set_label("");
            self.undo_btn.set_sensitive(false);
            self.redo_btn.set_sensitive(false);
            self.export_btn.set_sensitive(false);
            self.enable_document_actions(false);
            clear_box(&self.tool_options);
            *self.crop_label.borrow_mut() = None;
            self.layers.clear();
        }
        self.suppress.set(false);
        self.canvas.queue_draw();
    }

    fn enable(&self, name: &str, enabled: bool) {
        if let Some(action) = self.window.lookup_action(name) {
            if let Some(action) = action.downcast_ref::<gio::SimpleAction>() {
                action.set_enabled(enabled);
            }
        }
    }

    fn enable_document_actions(&self, enabled: bool) {
        for name in [
            "save",
            "save-as",
            "export",
            "close",
            "canvas-size",
            "image-size",
            "rotate-cw",
            "rotate-ccw",
            "rotate-180",
            "flip-h",
            "flip-v",
            "add-layer",
            "place",
            "duplicate",
            "delete-layer",
            "rotate-layer",
            "flip-layer-h",
            "flip-layer-v",
            "raise",
            "lower",
            "fit",
            "actual",
            "tool-move",
            "tool-crop",
            "undo",
            "redo",
        ] {
            if name == "undo" || name == "redo" {
                continue;
            }
            self.enable(name, enabled);
        }
    }

    fn fill_tool_options(self: &Rc<Self>) {
        clear_box(&self.tool_options);
        *self.crop_label.borrow_mut() = None;
        let model = self.model.borrow();
        let Some(session) = model.session.as_ref() else {
            return;
        };
        match session.tool {
            Tool::Move => {
                self.tool_options.append(&gtk::Label::new(Some(
                    "Drag to move the active layer. Arrow keys nudge 1 px, Shift nudges 10.",
                )));
            }
            Tool::Crop => {
                let label = gtk::Label::new(Some(&crop_hint(session.crop)));
                label.set_xalign(0.0);
                self.tool_options.append(&label);
                *self.crop_label.borrow_mut() = Some(label);
                let apply = gtk::Button::with_label("Apply crop");
                apply.add_css_class("suggested-action");
                apply.set_sensitive(session.crop.is_some());
                let shell = self.clone();
                apply.connect_clicked(move |_| shell.apply_crop());
                let cancel = gtk::Button::with_label("Cancel");
                cancel.set_sensitive(session.crop.is_some());
                let shell = self.clone();
                cancel.connect_clicked(move |_| shell.cancel_crop());
                self.tool_options.append(&apply);
                self.tool_options.append(&cancel);
            }
        }
    }

    fn refresh_cursor_name(&self) {
        let name = self
            .model
            .borrow()
            .session
            .as_ref()
            .map(|session| {
                if session.space_down {
                    "grab"
                } else {
                    match session.tool {
                        Tool::Move => "grab",
                        Tool::Crop => "crosshair",
                    }
                }
            })
            .unwrap_or("default");
        self.canvas.set_cursor_from_name(Some(name));
    }

    fn set_tool(self: &Rc<Self>, tool: Tool) {
        if self.suppress.get() {
            return;
        }
        {
            let mut model = self.model.borrow_mut();
            let Some(session) = model.session.as_mut() else {
                return;
            };
            if session.tool == tool {
                return;
            }
            session.tool = tool;
            if !matches!(tool, Tool::Crop) {
                session.crop = None;
            }
        }
        self.refresh();
    }

    fn show_document(self: &Rc<Self>, editor: Editor, path: Option<PathBuf>) {
        self.model.borrow_mut().open_editor(editor, path);
        let (width, height) = (self.canvas.width(), self.canvas.height());
        if width > 1 && height > 1 {
            if let Some(session) = self.model.borrow_mut().session.as_mut() {
                fit_view(session, width, height);
                session.fit_pending = false;
            }
        }
        self.refresh();
        self.canvas.grab_focus();
    }

    fn confirm(self: &Rc<Self>, next: Next) {
        let dirty = self
            .model
            .borrow()
            .session
            .as_ref()
            .is_some_and(|session| session.dirty);
        if !dirty {
            self.finish(next);
            return;
        }
        let dialog = libadwaita::AlertDialog::new(
            Some("Save changes?"),
            Some("This document has unsaved changes."),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("discard", "Discard");
        dialog.add_response("save", "Save");
        dialog.set_response_appearance("discard", libadwaita::ResponseAppearance::Destructive);
        dialog.set_response_appearance("save", libadwaita::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let shell = self.clone();
        dialog.choose(
            Some(&self.window.clone().upcast::<gtk::Widget>()),
            None::<&gio::Cancellable>,
            move |response| match response.as_str() {
                "discard" => shell.finish(next),
                "save" => {
                    let shell = shell.clone();
                    shell.save(move |shell| shell.finish(next));
                }
                _ => {}
            },
        );
    }

    fn finish(self: &Rc<Self>, next: Next) {
        match next {
            Next::Quit => {
                self.force_close.set(true);
                self.window.close();
            }
            Next::Run(callback) => callback(self),
        }
    }

    fn new_canvas(self: &Rc<Self>) {
        self.confirm(Next::Run(Box::new(move |shell| {
            let shell = shell.clone();
            let window = shell.window.clone();
            dialogs::new_canvas(&window, move |spec: NewCanvas| match Document::new(spec) {
                Ok(doc) => shell.show_document(Editor::new(doc), None),
                Err(err) => shell.toast(&err.to_string()),
            });
        })));
    }

    fn open_image(self: &Rc<Self>) {
        self.confirm(Next::Run(Box::new(move |shell| {
            shell.pick_file(
                "Open image",
                image_filter(),
                |shell, path| match open_image(&path) {
                    Ok(doc) => shell.show_document(Editor::new(doc), None),
                    Err(err) => shell.toast(&err.to_string()),
                },
            );
        })));
    }

    fn open_project(self: &Rc<Self>) {
        self.confirm(Next::Run(Box::new(move |shell| {
            shell.pick_file(
                "Open project",
                project_filter(),
                |shell, path| match open_project(&path) {
                    Ok(doc) => shell.show_document(Editor::new(doc), Some(path)),
                    Err(err) => shell.toast(&err.to_string()),
                },
            );
        })));
    }

    pub fn open_path(self: &Rc<Self>, path: &Path) {
        let path = path.to_path_buf();
        self.confirm(Next::Run(Box::new(move |shell| shell.load_path(&path))));
    }

    fn load_path(self: &Rc<Self>, path: &Path) {
        let result = if path.extension().and_then(|ext| ext.to_str()) == Some("pixel") {
            open_project(path).map(|doc| (doc, Some(path.to_path_buf())))
        } else {
            open_image(path).map(|doc| (doc, None))
        };
        match result {
            Ok((doc, project)) => self.show_document(Editor::new(doc), project),
            Err(err) => self.toast(&err.to_string()),
        }
    }

    fn close_document(self: &Rc<Self>) {
        self.confirm(Next::Run(Box::new(|shell| {
            shell.model.borrow_mut().session = None;
            shell.refresh();
        })));
    }

    fn save(self: &Rc<Self>, then: impl FnOnce(&Rc<Shell>) + 'static) {
        let existing = self
            .model
            .borrow()
            .session
            .as_ref()
            .and_then(|session| session.path.clone());
        if let Some(path) = existing {
            if self.write_project(&path) {
                self.refresh();
                then(self);
            }
        } else {
            self.save_as(then);
        }
    }

    fn save_as(self: &Rc<Self>, then: impl FnOnce(&Rc<Shell>) + 'static) {
        let name = self
            .model
            .borrow()
            .session
            .as_ref()
            .and_then(|session| session.path.clone())
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "Untitled.pixel".into());
        let dialog = gtk::FileDialog::new();
        dialog.set_title("Save project");
        dialog.set_initial_name(Some(&name));
        let filter = project_filter();
        dialog.set_default_filter(Some(&filter));
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));
        let shell = self.clone();
        let then = RefCell::new(Some(then));
        dialog.save(
            Some(&self.window),
            None::<&gio::Cancellable>,
            move |result| match result {
                Ok(file) => {
                    if let Some(path) = file.path() {
                        if shell.write_project(&path) {
                            shell.refresh();
                            if let Some(callback) = then.borrow_mut().take() {
                                callback(&shell);
                            }
                        }
                    }
                }
                Err(err) if dialog_cancelled(&err) => {}
                Err(err) => shell.toast(&err.to_string()),
            },
        );
    }

    fn write_project(&self, path: &Path) -> bool {
        let result = {
            let model = self.model.borrow();
            let Some(session) = model.session.as_ref() else {
                return false;
            };
            save_project(session.editor.document(), path)
        };
        match result {
            Ok(()) => {
                let mut model = self.model.borrow_mut();
                if let Some(session) = model.session.as_mut() {
                    session.path = Some(path.to_path_buf());
                    session.dirty = false;
                }
                self.toast("Saved");
                true
            }
            Err(err) => {
                self.toast(&err.to_string());
                false
            }
        }
    }

    fn export(self: &Rc<Self>) {
        let flattens = self
            .model
            .borrow()
            .session
            .as_ref()
            .is_some_and(|session| jpeg_needs_white(session.editor.document()));
        let shell = self.clone();
        dialogs::export_options(&self.window, flattens, move |format| {
            let ext = match format {
                ExportFormat::Png => "png",
                ExportFormat::Jpeg { .. } => "jpg",
                ExportFormat::Webp => "webp",
            };
            let stem = shell
                .model
                .borrow()
                .session
                .as_ref()
                .and_then(|session| session.path.as_ref())
                .and_then(|path| path.file_stem())
                .and_then(|stem| stem.to_str())
                .unwrap_or("image")
                .to_string();
            let dialog = gtk::FileDialog::new();
            dialog.set_title("Export image");
            dialog.set_initial_name(Some(&format!("{stem}.{ext}")));
            let shell_save = shell.clone();
            dialog.save(
                Some(&shell.window),
                None::<&gio::Cancellable>,
                move |result| match result {
                    Ok(file) => {
                        if let Some(path) = file.path() {
                            shell_save.write_export(&path, format);
                        }
                    }
                    Err(err) if dialog_cancelled(&err) => {}
                    Err(err) => shell_save.toast(&err.to_string()),
                },
            );
        });
    }

    fn write_export(&self, path: &Path, format: ExportFormat) {
        let result = {
            let model = self.model.borrow();
            let Some(session) = model.session.as_ref() else {
                return;
            };
            export(session.editor.document(), path, format)
        };
        match result {
            Ok(()) => self.toast("Exported"),
            Err(err) => self.toast(&err.to_string()),
        }
    }

    fn place_image(self: &Rc<Self>) {
        self.pick_file("Add image as layer", image_filter(), |shell, path| {
            let image = match ImageReader::open(&path)
                .map_err(|err| err.to_string())
                .and_then(|reader| reader.decode().map_err(|err| err.to_string()))
            {
                Ok(image) => image.into_rgba8(),
                Err(err) => {
                    shell.toast(&err);
                    return;
                }
            };
            let name = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("Image")
                .to_string();
            shell.edit(Command::AddImageLayer { name, image });
        });
    }

    fn pick_file(
        self: &Rc<Self>,
        title: &str,
        filter: gtk::FileFilter,
        on_ok: impl Fn(&Rc<Shell>, PathBuf) + 'static,
    ) {
        let dialog = gtk::FileDialog::new();
        dialog.set_title(title);
        dialog.set_default_filter(Some(&filter));
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        dialog.set_filters(Some(&filters));
        let shell = self.clone();
        let on_ok = RefCell::new(Some(on_ok));
        dialog.open(
            Some(&self.window),
            None::<&gio::Cancellable>,
            move |result| match result {
                Ok(file) => {
                    if let Some(path) = file.path() {
                        if let Some(callback) = on_ok.borrow_mut().take() {
                            callback(&shell, path);
                        }
                    }
                }
                Err(err) if dialog_cancelled(&err) => {}
                Err(err) => shell.toast(&err.to_string()),
            },
        );
    }

    fn canvas_size(self: &Rc<Self>) {
        let Some((width, height)) = self.doc_size() else {
            return;
        };
        let shell = self.clone();
        dialogs::canvas_size(&self.window, width, height, move |width, height, anchor| {
            shell.edit(Command::ResizeCanvas {
                width,
                height,
                anchor,
            });
        });
    }

    fn image_size(self: &Rc<Self>) {
        let Some((width, height)) = self.doc_size() else {
            return;
        };
        let shell = self.clone();
        dialogs::image_size(&self.window, width, height, move |width, height, filter| {
            shell.edit(Command::ScaleDocument {
                width,
                height,
                filter,
            });
        });
    }

    fn rotate_layer(self: &Rc<Self>) {
        let Some(index) = self.active_index() else {
            return;
        };
        let shell = self.clone();
        dialogs::rotate_layer(&self.window, move |degrees| {
            shell.edit(Command::RotateLayer {
                index,
                degrees_cw: degrees,
            });
        });
    }

    fn doc_size(&self) -> Option<(u32, u32)> {
        self.model.borrow().session.as_ref().map(|session| {
            let doc = session.editor.document();
            (doc.width, doc.height)
        })
    }

    fn reorder_active(self: &Rc<Self>, delta: isize) {
        let Some((from, len)) = self.model.borrow().session.as_ref().map(|session| {
            (
                session.editor.document().active_index(),
                session.editor.document().layers().len(),
            )
        }) else {
            return;
        };
        let to = (from as isize + delta).clamp(0, len as isize - 1) as usize;
        self.edit(Command::Reorder { from, to });
    }

    fn undo(self: &Rc<Self>) {
        self.step(true);
    }

    fn redo(self: &Rc<Self>) {
        self.step(false);
    }

    fn step(self: &Rc<Self>, undo: bool) {
        let changed = {
            let mut model = self.model.borrow_mut();
            let Some(session) = model.session.as_mut() else {
                return;
            };
            let changed = if undo {
                session.editor.undo()
            } else {
                session.editor.redo()
            };
            if changed {
                session.dirty = true;
                session.preview = None;
                session.visual = session.visual.wrapping_add(1);
            }
            changed
        };
        if changed {
            self.refresh();
        }
    }

    fn zoom_fit(self: &Rc<Self>) {
        let (width, height) = (self.canvas.width(), self.canvas.height());
        if let Some(session) = self.model.borrow_mut().session.as_mut() {
            fit_view(session, width, height);
        }
        self.refresh();
    }

    fn zoom_actual(self: &Rc<Self>) {
        let (width, height) = (self.canvas.width() as f64, self.canvas.height() as f64);
        if let Some(session) = self.model.borrow_mut().session.as_mut() {
            let doc = session.editor.document();
            session.zoom = 1.0;
            session.pan_x = width / 2.0 - doc.width as f64 / 2.0;
            session.pan_y = height / 2.0 - doc.height as f64 / 2.0;
        }
        self.refresh();
    }

    fn apply_crop(self: &Rc<Self>) {
        let crop = self
            .model
            .borrow()
            .session
            .as_ref()
            .and_then(|session| session.crop);
        let Some(crop) = crop else {
            return;
        };
        if let Some(session) = self.model.borrow_mut().session.as_mut() {
            session.crop = None;
        }
        self.edit(Command::Crop {
            x: crop.x,
            y: crop.y,
            width: crop.width,
            height: crop.height,
        });
    }

    fn cancel_crop(self: &Rc<Self>) {
        let had = if let Some(session) = self.model.borrow_mut().session.as_mut() {
            let had = session.crop.take().is_some() || session.preview.take().is_some();
            if had {
                session.visual = session.visual.wrapping_add(1);
            }
            had
        } else {
            false
        };
        if had {
            self.refresh();
        }
    }

    fn on_key(self: &Rc<Self>, key: gdk::Key, mods: gdk::ModifierType) -> glib::Propagation {
        if self.typing() {
            return glib::Propagation::Proceed;
        }
        let step = if mods.contains(gdk::ModifierType::SHIFT_MASK) {
            10
        } else {
            1
        };
        match key {
            gdk::Key::Left => self.nudge(-step, 0),
            gdk::Key::Right => self.nudge(step, 0),
            gdk::Key::Up => self.nudge(0, -step),
            gdk::Key::Down => self.nudge(0, step),
            gdk::Key::Escape => self.cancel_crop(),
            gdk::Key::space => {
                if let Some(session) = self.model.borrow_mut().session.as_mut() {
                    session.space_down = true;
                }
                self.canvas.set_cursor_from_name(Some("grab"));
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    fn nudge(self: &Rc<Self>, dx: i32, dy: i32) {
        let Some((index, x, y)) = self.model.borrow().session.as_ref().map(|session| {
            let layer = session.editor.document().active_layer();
            (session.editor.document().active_index(), layer.x, layer.y)
        }) else {
            return;
        };
        self.edit(Command::MoveLayer {
            index,
            x: x + dx,
            y: y + dy,
        });
    }

    fn typing(&self) -> bool {
        let Some(focus) = gtk::prelude::RootExt::focus(self.window.upcast_ref::<gtk::Window>())
        else {
            return false;
        };
        if focus.is::<gtk::Text>() || focus.is::<gtk::SpinButton>() || focus.is::<gtk::Entry>() {
            return true;
        }
        focus
            .downcast_ref::<gtk::EditableLabel>()
            .is_some_and(|label| label.is_editing())
    }

    fn on_canvas(self: &Rc<Self>, event: CanvasInput) {
        match event {
            CanvasInput::Resize { width, height } => {
                let pending = self
                    .model
                    .borrow()
                    .session
                    .as_ref()
                    .is_some_and(|session| session.fit_pending);
                if pending && width > 1 && height > 1 {
                    if let Some(session) = self.model.borrow_mut().session.as_mut() {
                        fit_view(session, width, height);
                        session.fit_pending = false;
                    }
                    self.refresh();
                }
            }
            CanvasInput::Motion { x, y } => self.track_cursor(x, y),
            CanvasInput::Leave => {
                if let Some(session) = self.model.borrow_mut().session.as_mut() {
                    session.cursor = None;
                }
                self.status_cursor.set_label("—");
            }
            CanvasInput::Scroll { x, y, dy } => self.zoom_wheel(x, y, dy),
            CanvasInput::DragBegin { x, y, button } => self.begin_drag(x, y, button),
            CanvasInput::DragUpdate { x, y } => self.update_drag(x, y),
            CanvasInput::DragEnd { x, y } => self.end_drag(x, y),
        }
    }

    fn track_cursor(&self, x: f64, y: f64) {
        let text = {
            let mut model = self.model.borrow_mut();
            let Some(session) = model.session.as_mut() else {
                return;
            };
            let (cx, cy) = widget_to_doc(session, x, y);
            let doc = session.editor.document();
            session.cursor =
                if cx >= 0.0 && cy >= 0.0 && (cx as u32) < doc.width && (cy as u32) < doc.height {
                    Some((cx.floor() as i32, cy.floor() as i32))
                } else {
                    None
                };
            cursor_text(session.cursor)
        };
        self.status_cursor.set_label(&text);
    }

    fn zoom_wheel(&self, x: f64, y: f64, dy: f64) {
        let label = {
            let mut model = self.model.borrow_mut();
            let Some(session) = model.session.as_mut() else {
                return;
            };
            let factor = if dy > 0.0 { 1.0 / 1.1 } else { 1.1 };
            let (cx, cy) = widget_to_doc(session, x, y);
            session.zoom = (session.zoom * factor).clamp(0.05, 32.0);
            session.pan_x = x - cx * session.zoom;
            session.pan_y = y - cy * session.zoom;
            format!("{:.0}%", session.zoom * 100.0)
        };
        self.status_zoom.set_label(&label);
        self.canvas.queue_draw();
    }

    fn begin_drag(&self, x: f64, y: f64, button: u32) {
        let mut model = self.model.borrow_mut();
        let Some(session) = model.session.as_mut() else {
            return;
        };
        if button == gdk::BUTTON_MIDDLE || session.space_down {
            let drag = Drag {
                kind: DragKind::Pan {
                    pan_x: session.pan_x,
                    pan_y: session.pan_y,
                },
                origin_x: x,
                origin_y: y,
            };
            drop(model);
            *self.drag.borrow_mut() = Some(drag);
            return;
        }
        if button != gdk::BUTTON_PRIMARY {
            return;
        }
        let drag = match session.tool {
            Tool::Move => {
                let index = session.editor.document().active_index();
                let layer = session.editor.document().active_layer();
                Drag {
                    kind: DragKind::Move {
                        index,
                        x: layer.x,
                        y: layer.y,
                    },
                    origin_x: x,
                    origin_y: y,
                }
            }
            Tool::Crop => {
                let (ax, ay) = widget_to_doc(session, x, y);
                Drag {
                    kind: DragKind::Crop { ax, ay },
                    origin_x: x,
                    origin_y: y,
                }
            }
        };
        drop(model);
        *self.drag.borrow_mut() = Some(drag);
    }

    fn update_drag(self: &Rc<Self>, x: f64, y: f64) {
        let Some(drag) = self.drag.borrow().clone() else {
            return;
        };
        match drag.kind {
            DragKind::Pan { pan_x, pan_y } => {
                if let Some(session) = self.model.borrow_mut().session.as_mut() {
                    session.pan_x = pan_x + (x - drag.origin_x);
                    session.pan_y = pan_y + (y - drag.origin_y);
                }
                self.canvas.queue_draw();
            }
            DragKind::Move {
                index,
                x: lx,
                y: ly,
            } => {
                let (dx, dy) = {
                    let model = self.model.borrow();
                    let Some(session) = model.session.as_ref() else {
                        return;
                    };
                    let (cx, cy) = widget_to_doc(session, x, y);
                    let (ox, oy) = widget_to_doc(session, drag.origin_x, drag.origin_y);
                    ((cx - ox).round() as i32, (cy - oy).round() as i32)
                };
                let mut model = self.model.borrow_mut();
                if let Some(session) = model.session.as_mut() {
                    session.preview = Some(Preview::Move {
                        index,
                        x: lx + dx,
                        y: ly + dy,
                    });
                    session.visual = session.visual.wrapping_add(1);
                }
                drop(model);
                self.canvas.queue_draw();
            }
            DragKind::Crop { ax, ay } => {
                let draft = {
                    let model = self.model.borrow();
                    let Some(session) = model.session.as_ref() else {
                        return;
                    };
                    let (bx, by) = widget_to_doc(session, x, y);
                    let doc = session.editor.document();
                    crop_from_points(ax, ay, bx, by, doc.width, doc.height)
                };
                if let Some(session) = self.model.borrow_mut().session.as_mut() {
                    session.crop = Some(draft);
                }
                if let Some(label) = self.crop_label.borrow().as_ref() {
                    label.set_label(&crop_hint(Some(draft)));
                }
                self.canvas.queue_draw();
            }
        }
    }

    fn end_drag(self: &Rc<Self>, x: f64, y: f64) {
        let Some(drag) = self.drag.borrow_mut().take() else {
            return;
        };
        match drag.kind {
            DragKind::Move {
                index,
                x: lx,
                y: ly,
            } => {
                let (dx, dy) = {
                    let model = self.model.borrow();
                    let Some(session) = model.session.as_ref() else {
                        return;
                    };
                    let (cx, cy) = widget_to_doc(session, x, y);
                    let (ox, oy) = widget_to_doc(session, drag.origin_x, drag.origin_y);
                    ((cx - ox).round() as i32, (cy - oy).round() as i32)
                };
                if let Some(session) = self.model.borrow_mut().session.as_mut() {
                    session.preview = None;
                }
                self.edit(Command::MoveLayer {
                    index,
                    x: lx + dx,
                    y: ly + dy,
                });
            }
            DragKind::Crop { .. } => self.refresh(),
            DragKind::Pan { .. } => self.refresh_cursor_name(),
        }
    }

    fn toast(&self, message: &str) {
        self.toasts.add_toast(libadwaita::Toast::new(message));
    }
}

fn install_theme(shell: &Rc<Shell>) {
    let provider = gtk::CssProvider::new();
    let shell_for_css = shell.clone();
    let provider_for_css = provider.clone();
    let apply = move || {
        let colors = ThemeColors::load();
        provider_for_css.load_from_string(&colors.css());
        let mut model = shell_for_css.model.borrow_mut();
        model.accent = colors.accent_rgb;
        model.background = colors.background_rgb;
        drop(model);
        shell_for_css.canvas.queue_draw();
    };
    apply();
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    if let Some(path) = theme::colors_path() {
        let file = gio::File::for_path(path);
        if let Ok(monitor) =
            file.monitor_file(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>)
        {
            let watched = shell.clone();
            let provider = provider.clone();
            monitor.connect_changed(move |_, _, _, _| {
                let colors = ThemeColors::load();
                provider.load_from_string(&colors.css());
                let mut model = watched.model.borrow_mut();
                model.accent = colors.accent_rgb;
                model.background = colors.background_rgb;
                drop(model);
                watched.canvas.queue_draw();
            });
            *shell.theme_monitor.borrow_mut() = Some(monitor);
        }
    }
}

fn welcome_page() -> (
    libadwaita::StatusPage,
    gtk::Button,
    gtk::Button,
    gtk::Button,
) {
    let page = libadwaita::StatusPage::new();
    page.set_title("Pixel");
    page.set_description(Some(
        "Set up a canvas, stack layers, and export a flat image.",
    ));
    let new_canvas = gtk::Button::with_label("New canvas");
    let open_image = gtk::Button::with_label("Open image");
    let open_project = gtk::Button::with_label("Open project");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::Center);
    row.append(&new_canvas);
    row.append(&open_image);
    row.append(&open_project);
    page.set_child(Some(&row));
    (page, new_canvas, open_image, open_project)
}

fn app_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append_submenu(Some("File"), &{
        let menu = gio::Menu::new();
        menu.append(Some("New Canvas"), Some("win.new"));
        menu.append(Some("Open Image"), Some("win.open"));
        menu.append(Some("Open Project"), Some("win.open-project"));
        menu.append(Some("Save"), Some("win.save"));
        menu.append(Some("Save As"), Some("win.save-as"));
        menu.append(Some("Export…"), Some("win.export"));
        menu.append(Some("Close"), Some("win.close"));
        menu.append(Some("Quit"), Some("win.quit"));
        menu
    });
    menu.append_submenu(Some("Edit"), &{
        let menu = gio::Menu::new();
        menu.append(Some("Undo"), Some("win.undo"));
        menu.append(Some("Redo"), Some("win.redo"));
        menu
    });
    menu.append_submenu(Some("Image"), &{
        let menu = gio::Menu::new();
        menu.append(Some("Canvas Size…"), Some("win.canvas-size"));
        menu.append(Some("Image Size…"), Some("win.image-size"));
        menu.append(Some("Rotate 90° Clockwise"), Some("win.rotate-cw"));
        menu.append(Some("Rotate 90° Counterclockwise"), Some("win.rotate-ccw"));
        menu.append(Some("Rotate 180°"), Some("win.rotate-180"));
        menu.append(Some("Flip Horizontal"), Some("win.flip-h"));
        menu.append(Some("Flip Vertical"), Some("win.flip-v"));
        menu
    });
    menu.append_submenu(Some("Layer"), &{
        let menu = gio::Menu::new();
        menu.append(Some("Add Layer"), Some("win.add-layer"));
        menu.append(Some("Add Image as Layer…"), Some("win.place"));
        menu.append(Some("Duplicate"), Some("win.duplicate"));
        menu.append(Some("Delete"), Some("win.delete-layer"));
        menu.append(Some("Rotate…"), Some("win.rotate-layer"));
        menu.append(Some("Flip Horizontal"), Some("win.flip-layer-h"));
        menu.append(Some("Flip Vertical"), Some("win.flip-layer-v"));
        menu.append(Some("Raise"), Some("win.raise"));
        menu.append(Some("Lower"), Some("win.lower"));
        menu
    });
    menu.append_submenu(Some("View"), &{
        let menu = gio::Menu::new();
        menu.append(Some("Fit"), Some("win.fit"));
        menu.append(Some("Actual Size"), Some("win.actual"));
        menu.append(Some("Move Tool"), Some("win.tool-move"));
        menu.append(Some("Crop Tool"), Some("win.tool-crop"));
        menu
    });
    menu
}

fn tool_button(label: &str) -> gtk::ToggleButton {
    let button = gtk::ToggleButton::with_label(label);
    button.add_css_class("pixel-tool");
    button
}

fn clear_box(box_: &gtk::Box) {
    while let Some(child) = box_.first_child() {
        box_.remove(&child);
    }
}

fn cursor_text(cursor: Option<(i32, i32)>) -> String {
    match cursor {
        Some((x, y)) => format!("{x}, {y}"),
        None => "—".into(),
    }
}

fn crop_hint(crop: Option<CropDraft>) -> String {
    match crop {
        Some(crop) => format!(
            "Crop {} × {} at {}, {}",
            crop.width, crop.height, crop.x, crop.y
        ),
        None => "Drag on the canvas to choose a crop.".into(),
    }
}

fn crop_from_points(ax: f64, ay: f64, bx: f64, by: f64, doc_w: u32, doc_h: u32) -> CropDraft {
    let clamp_x = |value: f64| value.clamp(0.0, doc_w as f64);
    let clamp_y = |value: f64| value.clamp(0.0, doc_h as f64);
    let x0 = clamp_x(ax.min(bx)).floor() as i32;
    let y0 = clamp_y(ay.min(by)).floor() as i32;
    let x1 = clamp_x(ax.max(bx)).ceil() as i32;
    let y1 = clamp_y(ay.max(by)).ceil() as i32;
    CropDraft {
        x: x0,
        y: y0,
        width: (x1 - x0).max(1) as u32,
        height: (y1 - y0).max(1) as u32,
    }
}

fn image_filter() -> gtk::FileFilter {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Images"));
    filter.add_mime_type("image/png");
    filter.add_mime_type("image/jpeg");
    filter.add_mime_type("image/webp");
    filter.add_pattern("*.png");
    filter.add_pattern("*.jpg");
    filter.add_pattern("*.jpeg");
    filter.add_pattern("*.webp");
    filter
}

fn project_filter() -> gtk::FileFilter {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Pixel project"));
    filter.add_pattern("*.pixel");
    filter
}

fn dialog_cancelled(err: &glib::Error) -> bool {
    err.matches(gtk::DialogError::Dismissed) || err.matches(gtk::DialogError::Cancelled)
}

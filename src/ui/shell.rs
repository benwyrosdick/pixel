//! The editor window. Document changes go through [`Shell::edit`].

use super::canvas::{upload, Canvas, CanvasInput, RULER_SIZE};
use super::dialogs;
use super::layers::LayersPanel;
use super::model::{
    active_bounds, doc_to_widget, document_title, fit_view, jpeg_needs_white, layer_bounds,
    widget_to_doc, CropDraft, GuideDraft, Model, Preview, ShapeDraft, ShapeStyle, SnapLine, Tool,
};
use super::snap::{moving_bounds, snap_lines, x_lines, y_lines, SnapTargets, SNAP_DISTANCE};
use super::theme::{self, ThemeColors};
use gtk::gdk;
use gtk::{gio, glib, prelude::*};
use image::ImageReader;
use libadwaita::prelude::*;
use pixel::document::{
    adjust, clockwise_delta, composite, composite_layers, export, hit_handle, open_image,
    open_project, pointer_angle, resize_rect, save_project, snap_angle, Adjustment, Alignment,
    Axis, BlendMode, Command, Document, Editor, ExportFormat, Guides, Handle, LayerKind, NewCanvas,
    PixelRect, QuarterTurn, ScaleFilter, ShapeKind, TextAlign, TextSpec, HANDLE_RADIUS,
    ROTATE_OFFSET,
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
    select_tool: gtk::ToggleButton,
    move_tool: gtk::ToggleButton,
    crop_tool: gtk::ToggleButton,
    text_tool: gtk::ToggleButton,
    shape_tool: gtk::ToggleButton,
    undo_btn: gtk::Button,
    redo_btn: gtk::Button,
    export_btn: gtk::Button,
    suppress: Cell<bool>,
    force_close: Cell<bool>,
    drag: RefCell<Option<Drag>>,
    crop_label: RefCell<Option<gtk::Label>>,
    theme_monitor: RefCell<Option<gio::FileMonitor>>,
    /// The File ▸ Open Recent submenu, refilled when the recent list changes.
    recent_menu: gio::Menu,
    /// The welcome screen's recent files, and the group that holds them.
    recent_list: gtk::ListBox,
    recent_group: gtk::Box,
    /// The files behind the welcome screen's rows, in row order.
    recent_paths: RefCell<Vec<PathBuf>>,
    /// The latest adjustment waiting to be previewed. Sliders move faster
    /// than a large layer can be redrawn, so only the newest one runs.
    pending_adjustment: RefCell<Option<(usize, Adjustment)>>,
    adjustment_queued: Cell<bool>,
}

#[derive(Clone)]
struct Drag {
    kind: DragKind,
    origin_x: f64,
    origin_y: f64,
}

#[derive(Clone)]
enum DragKind {
    Pan {
        pan_x: f64,
        pan_y: f64,
    },
    Move {
        indices: Vec<usize>,
        /// What the moving layers draw, where the drag began.
        bounds: Option<PixelRect>,
        targets: SnapTargets,
    },
    /// A shape being drawn from the point `(ax, ay)`.
    Shape {
        ax: f64,
        ay: f64,
        targets: SnapTargets,
    },
    /// A guide dragged out of a ruler, or an existing one being moved.
    Guide {
        vertical: bool,
        from: Option<usize>,
        targets: SnapTargets,
    },
    /// A select-tool press. It becomes a drag box once the pointer travels.
    Select {
        ax: f64,
        ay: f64,
    },
    Resize {
        index: usize,
        handle: Handle,
        origin: PixelRect,
        targets: SnapTargets,
    },
    Rotate {
        index: usize,
        cx: f64,
        cy: f64,
        start_angle: f64,
    },
    Crop {
        ax: f64,
        ay: f64,
    },
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
            chrome: colors.chrome_rgb(),
            chrome_text: colors.chrome_text_rgb(),
            show_rulers: true,
            show_guides: true,
            snap: true,
            shape_style: ShapeStyle {
                kind: ShapeKind::Arrow,
                stroke: ANNOTATION_RED,
                stroke_width: 4.0,
                fill: None,
            },
            text_style: TextSpec {
                text: String::new(),
                font: "Sans Bold".into(),
                size: 32.0,
                color: ANNOTATION_RED,
                align: TextAlign::Left,
            },
        }));

        let canvas = Canvas::new();
        canvas.set_model(model.clone());
        let layers = LayersPanel::new();

        let select_tool = tool_button("Select", "select", "V");
        let move_tool = tool_button("Move", "move", "M");
        let crop_tool = tool_button("Crop", "crop", "C");
        let text_tool = tool_button("Text", "text", "T");
        let shape_tool = tool_button("Shape", "shape", "U");
        for tool in [&move_tool, &crop_tool, &text_tool, &shape_tool] {
            tool.set_group(Some(&select_tool));
        }
        select_tool.set_active(true);
        let tools = gtk::Box::new(gtk::Orientation::Vertical, 0);
        tools.add_css_class("pixel-panel");
        tools.set_width_request(76);
        tools.set_hexpand(false);
        tools.set_hexpand_set(true);
        tools.set_vexpand(true);
        tools.append(&select_tool);
        tools.append(&move_tool);
        tools.append(&crop_tool);
        tools.append(&text_tool);
        tools.append(&shape_tool);

        let tool_options = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        tool_options.add_css_class("pixel-toolbar");
        tool_options.set_margin_top(4);
        tool_options.set_margin_bottom(4);
        tool_options.set_margin_start(8);
        tool_options.set_margin_end(8);

        // The tool hints sit under the canvas, inside its pane, so they share
        // its width rather than running under the tools and layers.
        let canvas_pane = gtk::Box::new(gtk::Orientation::Vertical, 0);
        canvas_pane.append(&canvas);
        canvas_pane.append(&tool_options);

        let split = gtk::Paned::new(gtk::Orientation::Horizontal);
        split.add_css_class("pixel-split");
        split.set_hexpand(true);
        split.set_vexpand(true);
        split.set_wide_handle(true);
        split.set_resize_start_child(true);
        split.set_resize_end_child(false);
        split.set_shrink_start_child(true);
        split.set_shrink_end_child(false);
        split.set_start_child(Some(&canvas_pane));
        split.set_end_child(Some(&layers.root));
        let split_placed = Cell::new(false);
        split.connect_notify_local(Some("width"), move |split, _| {
            if split_placed.get() {
                return;
            }
            let width = split.width();
            if width < 480 {
                return;
            }
            split_placed.set(true);
            split.set_position(width - 260);
        });

        let editor = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        editor.append(&tools);
        editor.append(&split);

        let (welcome, new_canvas, open_image, open_project, recent_group, recent_list) =
            welcome_page();
        let stack = gtk::Stack::new();
        stack.add_named(&welcome, Some("welcome"));
        stack.add_named(&editor, Some("editor"));

        let title = gtk::Label::new(Some("Pixel"));
        let undo_btn = gtk::Button::from_icon_name("edit-undo-symbolic");
        undo_btn.set_tooltip_text(Some("Undo"));
        let redo_btn = gtk::Button::from_icon_name("edit-redo-symbolic");
        redo_btn.set_tooltip_text(Some("Redo"));
        let export_btn = gtk::Button::with_label("Export");
        // A menu bar, not a menu button. Each menu drops down under its own
        // title, so none of them slide off the edge of the window.
        let recent_menu = gio::Menu::new();
        let menu_bar = gtk::PopoverMenuBar::from_model(Some(&app_menu(&recent_menu)));
        menu_bar.add_css_class("pixel-menubar");

        let header = libadwaita::HeaderBar::new();
        header.pack_start(&undo_btn);
        header.pack_start(&redo_btn);
        header.pack_end(&export_btn);
        header.set_title_widget(Some(&title));

        let top = gtk::Box::new(gtk::Orientation::Vertical, 0);
        top.append(&header);
        top.append(&menu_bar);

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
            select_tool,
            move_tool,
            crop_tool,
            text_tool,
            shape_tool,
            undo_btn,
            redo_btn,
            export_btn,
            suppress: Cell::new(false),
            force_close: Cell::new(false),
            drag: RefCell::new(None),
            crop_label: RefCell::new(None),
            theme_monitor: RefCell::new(None),
            recent_menu,
            recent_list,
            recent_group,
            recent_paths: RefCell::new(Vec::new()),
            pending_adjustment: RefCell::new(None),
            adjustment_queued: Cell::new(false),
        });
        shell.bind(&new_canvas, &open_image, &open_project, &fit, &actual);
        shell.refresh();
        shell.refresh_recent();
        let weak = Rc::downgrade(&shell);
        gtk::RecentManager::default().connect_changed(move |_| {
            if let Some(shell) = weak.upgrade() {
                shell.refresh_recent();
            }
        });
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
        buttons
            .delete
            .connect_clicked(move |_| shell.delete_selected());
        let shell = self.clone();
        buttons.up.connect_clicked(move |_| shell.reorder_active(1));
        let shell = self.clone();
        buttons
            .down
            .connect_clicked(move |_| shell.reorder_active(-1));

        let shell = self.clone();
        self.select_tool.connect_toggled(move |button| {
            if button.is_active() {
                shell.set_tool(Tool::Select);
            }
        });
        let shell = self.clone();
        self.move_tool.connect_toggled(move |button| {
            if button.is_active() {
                shell.set_tool(Tool::Move);
            }
        });
        for (button, tool) in [
            (&self.crop_tool, Tool::Crop),
            (&self.text_tool, Tool::Text),
            (&self.shape_tool, Tool::Shape),
        ] {
            let shell = self.clone();
            button.connect_toggled(move |button| {
                if button.is_active() {
                    shell.set_tool(tool);
                }
            });
        }

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
        self.add_action("cut", &[], |shell| shell.cut_selection());
        self.add_action("copy", &[], |shell| {
            shell.copy_selection();
        });
        self.add_action("copy-image", &[], |shell| shell.copy_image());
        self.add_action("paste", &[], |shell| shell.paste());
        self.add_action("select-all", &[], |shell| {
            shell.set_selection((0..shell.layer_count()).collect())
        });
        self.add_action("deselect", &[], |shell| shell.set_selection(Vec::new()));
        self.add_action("redo", &["<primary><shift>z", "<primary>y"], |shell| {
            shell.redo()
        });
        self.add_action("canvas-size", &[], |shell| shell.canvas_size());
        self.add_action("image-size", &[], |shell| shell.image_size());
        self.add_action("trim", &[], |shell| shell.trim_to_content());
        let open_recent = gio::SimpleAction::new("open-recent", Some(glib::VariantTy::STRING));
        let shell = self.clone();
        open_recent.connect_activate(move |_, target| {
            if let Some(path) = target.and_then(|target| target.str()) {
                shell.open_path(Path::new(path));
            }
        });
        self.window.add_action(&open_recent);
        let shell = self.clone();
        self.recent_list.connect_row_activated(move |_, row| {
            let path = usize::try_from(row.index())
                .ok()
                .and_then(|index| shell.recent_paths.borrow().get(index).cloned());
            if let Some(path) = path {
                shell.open_path(&path);
            }
        });
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
        self.add_action("duplicate", &["<primary>j"], |shell| {
            if let Some(index) = shell.active_index() {
                shell.edit(Command::DuplicateLayer { index });
            }
        });
        self.add_action("delete-layer", &[], |shell| shell.delete_selected());
        self.add_action("rotate-layer", &[], |shell| shell.rotate_layer());
        for (name, _) in ADJUSTMENTS {
            self.add_action(name, &[], move |shell| shell.open_adjustment(name));
        }
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
        for (name, to) in ALIGNMENTS {
            self.add_action(name, &[], move |shell| {
                shell.arrange(|indices| Command::AlignLayers { indices, to })
            });
        }
        for (name, axis) in DISTRIBUTIONS {
            self.add_action(name, &[], move |shell| {
                shell.arrange(|indices| Command::DistributeLayers { indices, axis })
            });
        }
        self.add_action("group", &["<primary>g"], |shell| {
            let indices = shell.selected();
            if !indices.is_empty() {
                shell.edit(Command::Group { indices });
            }
        });
        self.add_action("ungroup", &["<primary><shift>g"], |shell| {
            if let Some(index) = shell.active_index() {
                shell.edit(Command::Ungroup { index });
            }
        });
        self.add_action("raise", &["<primary>bracketright"], |shell| {
            shell.reorder_active(1)
        });
        self.add_action("lower", &["<primary>bracketleft"], |shell| {
            shell.reorder_active(-1)
        });
        self.add_action("fit", &["<primary>0"], |shell| shell.zoom_fit());
        self.add_action("actual", &["<primary>1"], |shell| shell.zoom_actual());
        self.add_toggle("show-rulers", &["<primary>r"], true, |model, on| {
            model.show_rulers = on
        });
        self.add_toggle("show-guides", &["<primary>semicolon"], true, |model, on| {
            model.show_guides = on
        });
        self.add_toggle("snap", &["<primary><shift>semicolon"], true, |model, on| {
            model.snap = on
        });
        self.add_action("clear-guides", &[], |shell| {
            shell.edit(Command::SetGuides {
                guides: Guides::default(),
            })
        });
        self.add_action("zoom-in", &[], |shell| shell.zoom_step(ZOOM_STEP));
        self.add_action("zoom-out", &[], |shell| shell.zoom_step(1.0 / ZOOM_STEP));
        self.add_action("tool-select", &[], |shell| shell.set_tool(Tool::Select));
        self.add_action("tool-move", &[], |shell| shell.set_tool(Tool::Move));
        self.add_action("tool-crop", &[], |shell| shell.set_tool(Tool::Crop));
        self.add_action("tool-text", &[], |shell| shell.set_tool(Tool::Text));
        self.add_action("tool-shape", &[], |shell| shell.set_tool(Tool::Shape));
        self.add_action("edit-text", &[], |shell| {
            if let Some(index) = shell.active_index() {
                shell.edit_text(index);
            }
        });
        self.add_action("revert-adjustments", &[], |shell| {
            if let Some(index) = shell.active_index() {
                shell.edit(Command::RevertAdjustments { index });
            }
        });
        let remove = gio::SimpleAction::new("remove-adjustment", Some(glib::VariantTy::UINT32));
        let shell = self.clone();
        remove.connect_activate(move |_, target| {
            let (Some(index), Some(position)) =
                (shell.active_index(), target.and_then(|t| t.get::<u32>()))
            else {
                return;
            };
            shell.edit(Command::RemoveAdjustment {
                index,
                position: position as usize,
            });
        });
        self.window.add_action(&remove);
        self.add_action("rasterize", &[], |shell| {
            if let Some(index) = shell.active_index() {
                shell.edit(Command::Rasterize { index });
            }
        });

        // Application accelerators run before the focused widget sees a key,
        // so a letter or Ctrl+V would never reach a layer name being edited.
        // These run after it instead, once nothing else has used the key.
        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_propagation_phase(gtk::PropagationPhase::Bubble);
        for (action, keys) in EDITING_SHORTCUTS {
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(keys),
                Some(gtk::NamedAction::new(&format!("win.{action}"))),
            ));
        }
        self.window.add_controller(shortcuts);

        // File browsers on Wayland can offer a file drag only as a move. Pixel
        // reads the file and never deletes it, so a move drop is as safe as a
        // copy, and GTK still picks copy when the source offers both.
        let drop = gtk::DropTarget::new(
            glib::Type::INVALID,
            gdk::DragAction::COPY | gdk::DragAction::MOVE,
        );
        drop.set_types(&[gdk::FileList::static_type(), gdk::Texture::static_type()]);
        let shell = self.clone();
        drop.connect_drop(move |_, value, _, _| {
            let shell = shell.clone();
            if let Ok(files) = value.get::<gdk::FileList>() {
                let paths = files
                    .files()
                    .iter()
                    .filter_map(|file| file.path())
                    .collect();
                // Opening can ask to save first, which shouldn't happen mid-drop.
                glib::idle_add_local_once(move || shell.add_files(paths));
                true
            } else if let Ok(texture) = value.get::<gdk::Texture>() {
                glib::idle_add_local_once(move || {
                    shell.add_image("Dropped image", texture_image(&texture))
                });
                true
            } else {
                false
            }
        });
        self.stack.add_controller(drop);

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

    fn view(&self) -> View {
        let model = self.model.borrow();
        View {
            rulers: model.show_rulers,
            guides: model.show_guides,
            snap: model.snap,
            width: self.canvas.width() as f64,
            height: self.canvas.height() as f64,
        }
    }

    /// A View menu checkbox. `apply` receives the new state.
    fn add_toggle(
        self: &Rc<Self>,
        name: &str,
        accels: &[&str],
        initial: bool,
        apply: impl Fn(&mut Model, bool) + 'static,
    ) {
        let action = gio::SimpleAction::new_stateful(name, None, &initial.to_variant());
        let shell = self.clone();
        action.connect_activate(move |action, _| {
            let on = !action
                .state()
                .and_then(|state| state.get::<bool>())
                .unwrap_or(false);
            action.set_state(&on.to_variant());
            apply(&mut shell.model.borrow_mut(), on);
            shell.canvas.queue_draw();
        });
        self.window.add_action(&action);
        if let Some(app) = self.window.application() {
            app.set_accels_for_action(&format!("win.{name}"), accels);
        }
    }

    /// Show a checkbox's state without running its action.
    fn set_toggle(&self, name: &str, on: bool) {
        if let Some(action) = self.window.lookup_action(name) {
            if let Some(action) = action.downcast_ref::<gio::SimpleAction>() {
                action.set_state(&on.to_variant());
            }
        }
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

    /// `indices` run bottom to top, like [`Document::selected_indices`].
    /// A click on a layer's row: select it alone, or with `extend`, add it to
    /// the selection or take it out, as Shift-clicking on the canvas does.
    pub fn click_layer(self: &Rc<Self>, index: usize, extend: bool) {
        if !extend {
            self.set_selection(vec![index]);
            return;
        }
        let changed = self
            .model
            .borrow_mut()
            .session
            .as_mut()
            .is_some_and(|session| session.editor.toggle_selected(index).is_ok());
        if changed {
            self.refresh();
        }
    }

    pub fn set_selection(self: &Rc<Self>, indices: Vec<usize>) {
        let changed = {
            let mut model = self.model.borrow_mut();
            let Some(session) = model.session.as_mut() else {
                return;
            };
            if session.editor.document().selected_indices() == indices {
                return;
            }
            session.editor.set_selection(&indices).is_ok()
        };
        if changed {
            self.refresh();
        }
    }

    /// The one selected layer's box, for the position and size fields.
    pub fn active_geometry(&self) -> Option<PixelRect> {
        let model = self.model.borrow();
        let layer = model.session.as_ref()?.editor.document().active_layer()?;
        if matches!(layer.kind, LayerKind::Group { .. }) {
            return None;
        }
        Some(PixelRect {
            x: layer.x,
            y: layer.y,
            width: layer.width(),
            height: layer.height(),
        })
    }

    /// Put the one selected layer in `rect`, resampling it if the size changed.
    pub fn set_active_geometry(self: &Rc<Self>, rect: PixelRect) {
        let (Some(index), Some(current)) = (self.active_index(), self.active_geometry()) else {
            return;
        };
        if (rect.width, rect.height) == (current.width, current.height) {
            self.edit(Command::MoveLayer {
                index,
                x: rect.x,
                y: rect.y,
            });
        } else {
            self.edit(Command::ScaleLayer {
                index,
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
                filter: ScaleFilter::Lanczos3,
            });
        }
    }

    /// Grayscale applies at once. The others open a dialog that previews on
    /// the canvas until it is applied or cancelled.
    fn open_adjustment(self: &Rc<Self>, name: &str) {
        let Some(index) = self.active_index() else {
            return;
        };
        let Some((sliders, make)) = adjustment_form(name) else {
            self.edit(Command::Adjust {
                index,
                adjustment: Adjustment::Grayscale,
            });
            return;
        };
        let title = ADJUSTMENTS
            .iter()
            .find(|(action, _)| *action == name)
            .map_or(name, |(_, label)| label.trim_end_matches('…'));
        let (preview, apply, cancel) = (self.clone(), self.clone(), self.clone());
        dialogs::adjustment(
            &self.window,
            title,
            &sliders,
            move |values| preview.preview_adjustment(index, make(values)),
            move |values| apply.apply_adjustment(index, make(values)),
            move || cancel.cancel_adjustment(),
        );
    }

    /// Show `adjustment` on the canvas without changing the document. Only
    /// the latest one is drawn, once the pending events are handled.
    fn preview_adjustment(self: &Rc<Self>, index: usize, adjustment: Adjustment) {
        *self.pending_adjustment.borrow_mut() = Some((index, adjustment));
        if self.adjustment_queued.replace(true) {
            return;
        }
        let shell = self.clone();
        glib::idle_add_local_once(move || {
            shell.adjustment_queued.set(false);
            let Some((index, adjustment)) = shell.pending_adjustment.borrow_mut().take() else {
                return;
            };
            if let Some(session) = shell.model.borrow_mut().session.as_mut() {
                let Some(layer) = session.editor.document().layers().get(index) else {
                    return;
                };
                let pixels = adjust(&layer.pixels, adjustment);
                session.preview = Some(Preview::Pixels { index, pixels });
                session.visual = session.visual.wrapping_add(1);
            }
            shell.canvas.queue_draw();
        });
    }

    fn apply_adjustment(self: &Rc<Self>, index: usize, adjustment: Adjustment) {
        self.pending_adjustment.borrow_mut().take();
        if let Some(session) = self.model.borrow_mut().session.as_mut() {
            session.preview = None;
        }
        self.edit(Command::Adjust { index, adjustment });
    }

    fn cancel_adjustment(&self) {
        self.pending_adjustment.borrow_mut().take();
        if let Some(session) = self.model.borrow_mut().session.as_mut() {
            if matches!(session.preview, Some(Preview::Pixels { .. })) {
                session.preview = None;
                session.visual = session.visual.wrapping_add(1);
            }
        }
        self.canvas.queue_draw();
    }

    /// A Text tool click: change the text layer under the pointer, or add text
    /// with its top-left corner there.
    fn text_at(self: &Rc<Self>, x: f64, y: f64) {
        let (hit, style) = {
            let model = self.model.borrow();
            let Some(session) = model.session.as_ref() else {
                return;
            };
            let doc = session.editor.document();
            let hit = doc.layer_at(x, y).filter(|&index| {
                let layer = &doc.layers()[index];
                !layer.locked && matches!(layer.kind, LayerKind::Text { .. })
            });
            (hit, model.text_style.clone())
        };
        if let Some(index) = hit {
            self.set_selection(vec![index]);
            self.edit_text(index);
            return;
        }
        let shell = self.clone();
        dialogs::text(&self.window, "Add text", "Add", &style, move |spec| {
            shell.remember_text_style(&spec);
            if spec.text.trim().is_empty() {
                return;
            }
            shell.edit(Command::AddText {
                spec,
                x: x.round() as i32,
                y: y.round() as i32,
            });
        });
    }

    fn edit_text(self: &Rc<Self>, index: usize) {
        let spec = {
            let model = self.model.borrow();
            let layer = model
                .session
                .as_ref()
                .and_then(|session| session.editor.document().layers().get(index));
            match layer.map(|layer| &layer.kind) {
                Some(LayerKind::Text { spec, .. }) => spec.clone(),
                _ => return,
            }
        };
        let shell = self.clone();
        dialogs::text(&self.window, "Edit text", "Apply", &spec, move |spec| {
            shell.remember_text_style(&spec);
            shell.edit(Command::SetText { index, spec });
        });
    }

    /// New text starts in the style last used.
    fn remember_text_style(&self, spec: &TextSpec) {
        self.model.borrow_mut().text_style = TextSpec {
            text: String::new(),
            ..spec.clone()
        };
    }

    /// The Shape tool's options: the kind, stroke color and width, and fill.
    fn fill_shape_options(self: &Rc<Self>, style: ShapeStyle) {
        let kinds = [
            ShapeKind::Rectangle,
            ShapeKind::Ellipse,
            ShapeKind::Line,
            ShapeKind::Arrow,
        ];
        let kind = gtk::DropDown::from_strings(&["Rectangle", "Ellipse", "Line", "Arrow"]);
        kind.set_selected(kinds.iter().position(|&k| k == style.kind).unwrap_or(0) as u32);
        let stroke = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
        stroke.set_rgba(&dialogs::rgba_color(style.stroke));
        stroke.set_tooltip_text(Some("Stroke color"));
        let width = gtk::SpinButton::with_range(0.0, 200.0, 1.0);
        width.set_digits(0);
        width.set_value(style.stroke_width as f64);
        width.set_tooltip_text(Some("Stroke width, in pixels"));
        let fill_on = gtk::CheckButton::with_label("Fill");
        fill_on.set_active(style.fill.is_some());
        let fill = gtk::ColorDialogButton::new(Some(gtk::ColorDialog::new()));
        let [r, g, b, _] = style.stroke;
        fill.set_rgba(&dialogs::rgba_color(style.fill.unwrap_or([r, g, b, 64])));
        fill.set_tooltip_text(Some("Fill color, for rectangles and ellipses"));
        fill.set_sensitive(style.fill.is_some());

        let read: Rc<dyn Fn() -> ShapeStyle> = Rc::new({
            let (kind, stroke, width, fill_on, fill) = (
                kind.clone(),
                stroke.clone(),
                width.clone(),
                fill_on.clone(),
                fill.clone(),
            );
            move || ShapeStyle {
                kind: kinds[kind.selected() as usize % kinds.len()],
                stroke: dialogs::rgba_bytes(stroke.rgba()),
                stroke_width: width.value() as f32,
                fill: fill_on
                    .is_active()
                    .then(|| dialogs::rgba_bytes(fill.rgba())),
            }
        });
        // Restyling rebuilds these controls, so wait until each signal is done.
        let changed = {
            let shell = self.clone();
            let read = read.clone();
            move || {
                let shell = shell.clone();
                let style = read();
                glib::idle_add_local_once(move || shell.set_shape_style(style));
            }
        };
        let on_change = changed.clone();
        kind.connect_selected_notify(move |_| on_change());
        let on_change = changed.clone();
        stroke.connect_rgba_notify(move |_| on_change());
        let on_change = changed.clone();
        width.connect_value_changed(move |_| on_change());
        let on_change = changed.clone();
        fill.connect_rgba_notify(move |_| on_change());
        let fill_c = fill.clone();
        fill_on.connect_toggled(move |check| {
            fill_c.set_sensitive(check.is_active());
            changed();
        });

        let options = &self.tool_options;
        options.append(&kind);
        options.append(&gtk::Label::new(Some("Stroke")));
        options.append(&stroke);
        options.append(&width);
        options.append(&fill_on);
        options.append(&fill);
        options.append(&hint_label(
            "Drag to draw. Shift keeps squares, circles, and 45° angles. These options restyle the selected shape too.",
        ));
    }

    /// Use `style` for new shapes, and restyle the one selected shape.
    fn set_shape_style(self: &Rc<Self>, style: ShapeStyle) {
        let target = {
            let mut model = self.model.borrow_mut();
            model.shape_style = style;
            model.session.as_ref().and_then(|session| {
                let doc = session.editor.document();
                let layer = doc.active_layer().filter(|layer| !layer.locked)?;
                match &layer.kind {
                    LayerKind::Shape { spec, .. } => Some((doc.active_index()?, spec.dx, spec.dy)),
                    _ => None,
                }
            })
        };
        if let Some((index, dx, dy)) = target {
            self.edit(Command::SetShape {
                index,
                spec: style.spec(dx, dy),
            });
        }
    }

    /// Open a layer's right-click menu at `(x, y)` in `row`. The layer is
    /// selected first, unless it is already part of the selection.
    pub fn show_layer_menu(self: &Rc<Self>, index: usize, row: &gtk::Widget, x: f64, y: f64) {
        let root = self.layers.root.clone();
        let Some(point) = row.compute_point(&root, &gtk::graphene::Point::new(x as f32, y as f32))
        else {
            return;
        };
        let shell = self.clone();
        // Selecting rebuilds the rows, so finish with this click first.
        glib::idle_add_local_once(move || {
            let selected = shell.selected();
            if !selected.contains(&index) {
                shell.set_selection(vec![index]);
            }
            let menu = shell.layer_menu(index);
            let popover = gtk::PopoverMenu::from_model(Some(&menu));
            popover.set_parent(&root);
            popover.set_has_arrow(false);
            popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                point.x() as i32,
                point.y() as i32,
                1,
                1,
            )));
            popover.connect_closed(|popover| {
                let popover = popover.clone();
                glib::idle_add_local_once(move || popover.unparent());
            });
            popover.popup();
        });
    }

    /// The right-click menu for one layer: its adjustments, then the layer
    /// commands that apply to it.
    fn layer_menu(&self, index: usize) -> gio::Menu {
        let menu = gio::Menu::new();
        let model = self.model.borrow();
        let Some(doc) = model
            .session
            .as_ref()
            .map(|session| session.editor.document())
        else {
            return menu;
        };
        let Some(layer) = doc.layers().get(index) else {
            return menu;
        };
        // Adjustment commands act on the one selected layer, so they only
        // show when this is it.
        if doc.active_index() == Some(index) && !layer.adjustments.is_empty() {
            let section = gio::Menu::new();
            for (position, adjustment) in layer.adjustments.iter().enumerate() {
                let item = gio::MenuItem::new(
                    Some(&menu_label(&format!(
                        "Remove {}",
                        adjustment_label(adjustment)
                    ))),
                    None,
                );
                item.set_action_and_target_value(
                    Some("win.remove-adjustment"),
                    Some(&(position as u32).to_variant()),
                );
                section.append_item(&item);
            }
            section.append_item(&menu_item("Revert All Adjustments", "revert-adjustments"));
            menu.append_section(Some("Adjustments"), &section);
        }
        let section = gio::Menu::new();
        if matches!(layer.kind, LayerKind::Text { .. }) {
            section.append_item(&menu_item("Edit Text…", "edit-text"));
        }
        if layer.is_vector() {
            section.append_item(&menu_item("Rasterize", "rasterize"));
        }
        section.append_item(&menu_item("Duplicate", "duplicate"));
        section.append_item(&menu_item("Delete", "delete-layer"));
        if matches!(layer.kind, LayerKind::Group { .. }) {
            section.append_item(&menu_item("Ungroup", "ungroup"));
        } else {
            section.append_item(&menu_item("Group", "group"));
        }
        menu.append_section(None, &section);
        menu
    }

    pub fn set_active_blend(self: &Rc<Self>, blend: BlendMode) {
        if let Some(index) = self.active_index() {
            self.edit(Command::SetBlend { index, blend });
        }
    }

    /// Run an align or distribute command on the selection.
    fn arrange(self: &Rc<Self>, command: impl FnOnce(Vec<usize>) -> Command) {
        let indices = self
            .model
            .borrow()
            .session
            .as_ref()
            .map(|session| movable_selection(session.editor.document()))
            .unwrap_or_default();
        if !indices.is_empty() {
            self.edit(command(indices));
        }
    }

    /// Copy the selected layers, flattened and trimmed, or the whole image
    /// when nothing is selected. Returns whether anything was copied.
    fn copy_selection(self: &Rc<Self>) -> bool {
        let image = {
            let model = self.model.borrow();
            let Some(session) = model.session.as_ref() else {
                return false;
            };
            let doc = session.editor.document();
            let indices = doc.selected_indices();
            if indices.is_empty() {
                Some(composite(doc))
            } else {
                composite_layers(doc, &indices)
            }
        };
        let Some(image) = image else {
            self.toast("The selected layers have nothing visible to copy");
            return false;
        };
        self.window.clipboard().set_texture(&upload(&image));
        true
    }

    /// Copy the flattened image, background included, whatever is selected.
    fn copy_image(self: &Rc<Self>) {
        let image = self
            .model
            .borrow()
            .session
            .as_ref()
            .map(|session| composite(session.editor.document()));
        if let Some(image) = image {
            self.window.clipboard().set_texture(&upload(&image));
        }
    }

    fn cut_selection(self: &Rc<Self>) {
        let selected = self
            .model
            .borrow()
            .session
            .as_ref()
            .is_some_and(|session| !session.editor.document().selected_indices().is_empty());
        if selected && self.copy_selection() {
            self.delete_selected();
        }
    }

    /// Paste an image as a new layer, or copied image files as layers. With
    /// no document open, the image opens as one.
    fn paste(self: &Rc<Self>) {
        let clipboard = self.window.clipboard();
        let formats = clipboard.formats();
        let shell = self.clone();
        if formats.contains_type(gdk::Texture::static_type()) {
            glib::spawn_future_local(async move {
                match clipboard.read_texture_future().await {
                    Ok(Some(texture)) => shell.add_image("Pasted image", texture_image(&texture)),
                    Ok(None) => shell.toast("The clipboard has no image"),
                    Err(err) => shell.toast(&format!("Couldn't paste: {err}")),
                }
            });
        } else if formats.contains_type(gdk::FileList::static_type()) {
            glib::spawn_future_local(async move {
                let value = clipboard
                    .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
                    .await;
                match value.map(|value| value.get::<gdk::FileList>()) {
                    Ok(Ok(files)) => {
                        let paths = files
                            .files()
                            .iter()
                            .filter_map(|file| file.path())
                            .collect();
                        shell.add_files(paths);
                    }
                    _ => shell.toast("Couldn't read the copied files"),
                }
            });
        } else {
            self.toast("The clipboard has no image");
        }
    }

    /// Add an image as a new layer, or open it when no document is open.
    fn add_image(self: &Rc<Self>, name: &str, image: image::RgbaImage) {
        if self.model.borrow().session.is_some() {
            self.edit(Command::AddImageLayer {
                name: name.to_string(),
                image,
            });
            return;
        }
        match Document::from_image(image, 72.0) {
            Ok(doc) => self.show_document(Editor::new(doc), None),
            Err(err) => self.toast(&err.to_string()),
        }
    }

    /// Take in dropped or pasted files. A project replaces the open document.
    /// Images become layers, except that the first opens as the document when
    /// none is open.
    fn add_files(self: &Rc<Self>, paths: Vec<PathBuf>) {
        if let Some(project) = paths.iter().find(|path| is_project(path)) {
            self.open_path(project);
            return;
        }
        let mut images = paths.into_iter();
        if self.model.borrow().session.is_none() {
            let Some(first) = images.next() else {
                return;
            };
            self.load_path(&first);
        }
        for path in images {
            self.place_path(&path);
        }
    }

    /// The selected layers, bottom to top.
    fn selected(&self) -> Vec<usize> {
        self.model
            .borrow()
            .session
            .as_ref()
            .map(|session| session.editor.document().selected_indices())
            .unwrap_or_default()
    }

    fn delete_selected(self: &Rc<Self>) {
        let indices = self
            .model
            .borrow()
            .session
            .as_ref()
            .map(|session| session.editor.document().selected_indices())
            .unwrap_or_default();
        if !indices.is_empty() {
            self.edit(Command::DeleteLayers { indices });
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
            .and_then(|session| session.editor.document().active_index())
    }

    pub fn active_opacity(&self) -> Option<f32> {
        self.model
            .borrow()
            .session
            .as_ref()
            .and_then(|session| session.editor.document().active_layer())
            .map(|layer| layer.opacity)
    }

    pub fn preview_opacity(&self, opacity: f32) {
        let mut model = self.model.borrow_mut();
        let Some(session) = model.session.as_mut() else {
            return;
        };
        let Some(index) = session.editor.document().active_index() else {
            return;
        };
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
            self.select_tool
                .set_active(matches!(session.tool, Tool::Select));
            self.move_tool
                .set_active(matches!(session.tool, Tool::Move));
            self.crop_tool
                .set_active(matches!(session.tool, Tool::Crop));
            self.text_tool
                .set_active(matches!(session.tool, Tool::Text));
            self.shape_tool
                .set_active(matches!(session.tool, Tool::Shape));
            self.undo_btn.set_sensitive(session.editor.can_undo());
            self.redo_btn.set_sensitive(session.editor.can_redo());
            self.export_btn.set_sensitive(true);
            self.enable("undo", session.editor.can_undo());
            self.enable("redo", session.editor.can_redo());
            self.enable_document_actions(true);
            let active = doc.active_layer();
            for name in ["duplicate", "raise", "lower"] {
                self.enable(name, active.is_some());
            }
            self.enable("delete-layer", !doc.selected_indices().is_empty());
            self.enable("cut", !doc.selected_indices().is_empty());
            let movable = movable_selection(doc).len();
            for (name, _) in ALIGNMENTS {
                self.enable(name, movable >= 1);
            }
            for (name, _) in DISTRIBUTIONS {
                self.enable(name, movable >= 3);
            }
            let editable = active.is_some_and(|layer| {
                !layer.locked && !matches!(layer.kind, LayerKind::Group { .. })
            });
            self.enable("group", !doc.selected_indices().is_empty());
            self.enable(
                "ungroup",
                active.is_some_and(|layer| matches!(layer.kind, LayerKind::Group { .. })),
            );
            for name in ["rotate-layer", "flip-layer-h", "flip-layer-v"] {
                self.enable(name, editable);
            }
            for (name, _) in ADJUSTMENTS {
                self.enable(name, editable);
            }
            let unlocked = active.filter(|layer| !layer.locked);
            let text = unlocked.is_some_and(|layer| matches!(layer.kind, LayerKind::Text { .. }));
            self.enable("edit-text", text);
            self.enable("rasterize", unlocked.is_some_and(|layer| layer.is_vector()));
            let adjusted = unlocked.is_some_and(|layer| !layer.adjustments.is_empty());
            self.enable("revert-adjustments", adjusted);
            self.enable("remove-adjustment", adjusted);
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
            "trim",
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
            "brightness-contrast",
            "hue-saturation",
            "levels",
            "grayscale",
            "blur",
            "sharpen",
            "flip-layer-h",
            "flip-layer-v",
            "raise",
            "lower",
            "align-left",
            "align-horizontal-center",
            "align-right",
            "align-top",
            "align-vertical-center",
            "align-bottom",
            "distribute-horizontal",
            "distribute-vertical",
            "fit",
            "actual",
            "zoom-in",
            "zoom-out",
            "clear-guides",
            "cut",
            "copy",
            "copy-image",
            "select-all",
            "deselect",
            "tool-select",
            "tool-move",
            "tool-crop",
            "tool-text",
            "tool-shape",
            "group",
            "ungroup",
            "edit-text",
            "rasterize",
            "revert-adjustments",
            "remove-adjustment",
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
            Tool::Select => {
                self.tool_options.append(&hint_label(
                    "Click a layer to select it, or Shift-click to add or remove it. Drag a box around layers to select them, with Shift to add them.",
                ));
            }
            Tool::Move => {
                self.tool_options.append(&hint_label(
                    "Drag the selection to move it. With one layer selected, handles resize it and the round handle rotates it. Shift locks the aspect ratio, and snaps rotation to 45°. Arrow keys nudge 1 px, Shift nudges 10.",
                ));
            }
            Tool::Text => {
                self.tool_options.append(&hint_label(
                    "Click to add text, or click text to change it. Layer ▸ Edit Text… changes the selected text layer.",
                ));
            }
            Tool::Shape => {
                let style = match session
                    .editor
                    .document()
                    .active_layer()
                    .map(|layer| &layer.kind)
                {
                    Some(LayerKind::Shape { spec, .. }) => ShapeStyle::of(spec),
                    _ => model.shape_style,
                };
                drop(model);
                self.fill_shape_options(style);
                return;
            }
            Tool::Crop => {
                let label = hint_label(&crop_hint(session.crop));
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
                        Tool::Select | Tool::Move => "default",
                        Tool::Crop | Tool::Shape => "crosshair",
                        Tool::Text => "text",
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
            session.marquee = None;
            if !matches!(tool, Tool::Crop) {
                session.crop = None;
            }
        }
        self.refresh();
    }

    fn show_document(self: &Rc<Self>, editor: Editor, path: Option<PathBuf>) {
        self.model.borrow_mut().open_editor(editor, path);
        let (width, height) = (self.canvas.width(), self.canvas.height());
        let inset = self.view().inset();
        if width > 1 && height > 1 {
            if let Some(session) = self.model.borrow_mut().session.as_mut() {
                fit_view(session, width, height, inset);
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
            shell.pick_file("Open image", image_filter(), |shell, path| {
                shell.load_path(&path)
            });
        })));
    }

    fn open_project(self: &Rc<Self>) {
        self.confirm(Next::Run(Box::new(move |shell| {
            shell.pick_file("Open project", project_filter(), |shell, path| {
                shell.load_path(&path)
            });
        })));
    }

    pub fn open_path(self: &Rc<Self>, path: &Path) {
        let path = path.to_path_buf();
        self.confirm(Next::Run(Box::new(move |shell| shell.load_path(&path))));
    }

    fn load_path(self: &Rc<Self>, path: &Path) {
        let result = if is_project(path) {
            open_project(path).map(|doc| (doc, Some(path.to_path_buf())))
        } else {
            open_image(path).map(|doc| (doc, None))
        };
        match result {
            Ok((doc, project)) => {
                remember(path);
                self.show_document(Editor::new(doc), project);
            }
            Err(err) => self.toast(&err.to_string()),
        }
    }

    /// Crop the canvas to what the visible layers draw. The view shifts with
    /// the crop so the content stays where it was on screen.
    fn trim_to_content(self: &Rc<Self>) {
        let (bounds, full) = {
            let model = self.model.borrow();
            let Some(session) = model.session.as_ref() else {
                return;
            };
            let doc = session.editor.document();
            let full = PixelRect {
                x: 0,
                y: 0,
                width: doc.width,
                height: doc.height,
            };
            (doc.content_bounds(), full)
        };
        let Some(bounds) = bounds else {
            self.toast("There is nothing visible to trim to");
            return;
        };
        if bounds == full {
            self.toast("The canvas already fits its content");
            return;
        }
        if let Some(session) = self.model.borrow_mut().session.as_mut() {
            session.crop = None;
            session.pan_x += bounds.x as f64 * session.zoom;
            session.pan_y += bounds.y as f64 * session.zoom;
        }
        self.edit(Command::Crop {
            x: bounds.x,
            y: bounds.y,
            width: bounds.width,
            height: bounds.height,
        });
    }

    /// Refill the Open Recent submenu and the welcome screen's list.
    fn refresh_recent(&self) {
        let paths = recent_files();
        self.recent_menu.remove_all();
        for path in &paths {
            let item = gio::MenuItem::new(Some(&menu_label(&display_name(path))), None);
            item.set_action_and_target_value(
                Some("win.open-recent"),
                Some(&path.to_string_lossy().to_variant()),
            );
            self.recent_menu.append_item(&item);
        }
        if paths.is_empty() {
            // No action, so GTK shows it greyed out.
            self.recent_menu
                .append_item(&gio::MenuItem::new(Some("No Recent Files"), None));
        }
        while let Some(row) = self.recent_list.row_at_index(0) {
            self.recent_list.remove(&row);
        }
        let shown: Vec<PathBuf> = paths.iter().take(WELCOME_RECENT).cloned().collect();
        for path in &shown {
            self.recent_list.append(&recent_row(path));
        }
        *self.recent_paths.borrow_mut() = shown;
        self.recent_group.set_visible(!paths.is_empty());
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
                drop(model);
                remember(path);
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
            shell.place_path(&path)
        });
    }

    /// Add an image file as a new layer named after the file.
    fn place_path(self: &Rc<Self>, path: &Path) {
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Image")
            .to_string();
        let image = match ImageReader::open(path)
            .map_err(|err| err.to_string())
            .and_then(|reader| reader.decode().map_err(|err| err.to_string()))
        {
            Ok(image) => image.into_rgba8(),
            Err(err) => {
                self.toast(&format!("Couldn't open {name}: {err}"));
                return;
            }
        };
        self.edit(Command::AddImageLayer { name, image });
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

    /// Trade places with the next layer or group up or down, within the same
    /// group.
    fn reorder_active(self: &Rc<Self>, delta: isize) {
        let Some((from, to)) = self.model.borrow().session.as_ref().and_then(|session| {
            let doc = session.editor.document();
            let from = doc.active_index()?;
            Some((from, doc.sibling(from, delta > 0)?))
        }) else {
            return;
        };
        self.edit(Command::Reorder { from, to });
    }

    /// Fold a group away in the layers panel, or open it.
    pub fn set_collapsed(self: &Rc<Self>, index: usize, collapsed: bool) {
        let changed = self
            .model
            .borrow_mut()
            .session
            .as_mut()
            .is_some_and(|session| session.editor.set_collapsed(index, collapsed).is_ok());
        if changed {
            self.refresh();
        }
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
        let inset = self.view().inset();
        if let Some(session) = self.model.borrow_mut().session.as_mut() {
            fit_view(session, width, height, inset);
        }
        self.refresh();
    }

    fn zoom_actual(self: &Rc<Self>) {
        let (width, height) = (self.canvas.width() as f64, self.canvas.height() as f64);
        let inset = self.view().inset();
        if let Some(session) = self.model.borrow_mut().session.as_mut() {
            let doc = session.editor.document();
            session.zoom = 1.0;
            session.pan_x = inset + (width - inset - doc.width as f64) / 2.0;
            session.pan_y = inset + (height - inset - doc.height as f64) / 2.0;
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
            gdk::Key::Escape => {
                let cropping = self
                    .model
                    .borrow()
                    .session
                    .as_ref()
                    .is_some_and(|session| session.crop.is_some());
                if cropping {
                    self.cancel_crop();
                } else {
                    self.set_selection(Vec::new());
                }
            }
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
        let indices = self
            .model
            .borrow()
            .session
            .as_ref()
            .map(|session| movable_selection(session.editor.document()))
            .unwrap_or_default();
        if !indices.is_empty() {
            self.edit(Command::MoveLayers { indices, dx, dy });
        }
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
                    let inset = self.view().inset();
                    if let Some(session) = self.model.borrow_mut().session.as_mut() {
                        fit_view(session, width, height, inset);
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
            CanvasInput::DragUpdate { x, y, shift, ctrl } => self.update_drag(x, y, shift, ctrl),
            CanvasInput::DragEnd { x, y, shift, ctrl } => self.end_drag(x, y, shift, ctrl),
        }
    }

    fn track_cursor(&self, x: f64, y: f64) {
        let view = self.view();
        let (text, cursor) = {
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
            let cursor = hover_cursor(session, view, x, y);
            (cursor_text(session.cursor), cursor)
        };
        self.status_cursor.set_label(&text);
        if let Some(cursor) = cursor {
            self.canvas.set_cursor_from_name(Some(cursor));
        }
        if view.rulers {
            // Moves the pointer mark along the rulers.
            self.canvas.queue_draw();
        }
    }

    fn zoom_wheel(&self, x: f64, y: f64, dy: f64) {
        self.zoom_at(x, y, if dy > 0.0 { 1.0 / 1.1 } else { 1.1 });
    }

    /// Zoom about the middle of the canvas.
    fn zoom_step(&self, factor: f64) {
        let (x, y) = (self.canvas.width() as f64, self.canvas.height() as f64);
        self.zoom_at(x / 2.0, y / 2.0, factor);
    }

    /// Zoom by `factor`, keeping the document point under `(x, y)` in place.
    fn zoom_at(&self, x: f64, y: f64, factor: f64) {
        let label = {
            let mut model = self.model.borrow_mut();
            let Some(session) = model.session.as_mut() else {
                return;
            };
            let (cx, cy) = widget_to_doc(session, x, y);
            session.zoom = (session.zoom * factor).clamp(0.05, 32.0);
            session.pan_x = x - cx * session.zoom;
            session.pan_y = y - cy * session.zoom;
            format!("{:.0}%", session.zoom * 100.0)
        };
        self.status_zoom.set_label(&label);
        self.canvas.queue_draw();
    }

    fn begin_drag(self: &Rc<Self>, x: f64, y: f64, button: u32) {
        let view = self.view();
        let mut model = self.model.borrow_mut();
        let Some(session) = model.session.as_mut() else {
            return;
        };
        session.snap_lines.clear();
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
        if view.rulers && x < RULER_SIZE && y < RULER_SIZE {
            return;
        }
        if let Some((vertical, from)) = guide_pickup(session, view, x, y) {
            let doc = session.editor.document();
            let targets = SnapTargets::collect(doc, &[], false);
            let (px, py) = widget_to_doc(session, x, y);
            let position = match from {
                Some(index) if vertical => doc.guides().x[index] as f64,
                Some(index) => doc.guides().y[index] as f64,
                None if vertical => px.round(),
                None => py.round(),
            };
            session.guide_draft = Some(GuideDraft {
                vertical,
                position,
                from,
                removing: from.is_none(),
            });
            // A guide pulled out while guides are hidden brings them back.
            model.show_guides = true;
            drop(model);
            if !view.guides {
                self.set_toggle("show-guides", true);
            }
            *self.drag.borrow_mut() = Some(Drag {
                kind: DragKind::Guide {
                    vertical,
                    from,
                    targets,
                },
                origin_x: x,
                origin_y: y,
            });
            self.canvas.queue_draw();
            return;
        }
        let mut selected = false;
        let drag = match session.tool {
            Tool::Select => {
                let (ax, ay) = widget_to_doc(session, x, y);
                Drag {
                    kind: DragKind::Select { ax, ay },
                    origin_x: x,
                    origin_y: y,
                }
            }
            Tool::Move => {
                selected = select_under_pointer(session, x, y);
                let Some(kind) = move_drag(session, x, y, view.guides) else {
                    drop(model);
                    if selected {
                        self.refresh();
                    }
                    return;
                };
                Drag {
                    kind,
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
            Tool::Text => {
                let (px, py) = widget_to_doc(session, x, y);
                drop(model);
                // The dialog shouldn't open inside the press it came from.
                let shell = self.clone();
                glib::idle_add_local_once(move || shell.text_at(px, py));
                return;
            }
            Tool::Shape => {
                let targets = SnapTargets::collect(session.editor.document(), &[], view.guides);
                let (px, py) = widget_to_doc(session, x, y);
                let (ax, ay) = snap_point(session, &targets, px, py, view.snap).0;
                Drag {
                    kind: DragKind::Shape { ax, ay, targets },
                    origin_x: x,
                    origin_y: y,
                }
            }
        };
        drop(model);
        *self.drag.borrow_mut() = Some(drag);
        if selected {
            self.refresh();
        }
    }

    fn update_drag(self: &Rc<Self>, x: f64, y: f64, shift: bool, ctrl: bool) {
        let Some(drag) = self.drag.borrow().clone() else {
            return;
        };
        let view = self.view();
        let snap = view.snap && !ctrl;
        match drag.kind {
            DragKind::Pan { pan_x, pan_y } => {
                if let Some(session) = self.model.borrow_mut().session.as_mut() {
                    session.pan_x = pan_x + (x - drag.origin_x);
                    session.pan_y = pan_y + (y - drag.origin_y);
                }
                self.canvas.queue_draw();
            }
            DragKind::Resize {
                index,
                handle,
                origin,
                targets,
            } => {
                let mut model = self.model.borrow_mut();
                if let Some(session) = model.session.as_mut() {
                    let (rect, lines) =
                        snapped_resize(session, origin, handle, &targets, x, y, shift, snap);
                    session.snap_lines = lines;
                    session.preview = Some(Preview::Resize {
                        index,
                        x: rect.x,
                        y: rect.y,
                        width: rect.width,
                        height: rect.height,
                    });
                    session.visual = session.visual.wrapping_add(1);
                }
                drop(model);
                self.canvas.queue_draw();
            }
            DragKind::Rotate {
                index,
                cx,
                cy,
                start_angle,
            } => {
                let degrees = {
                    let model = self.model.borrow();
                    let Some(session) = model.session.as_ref() else {
                        return;
                    };
                    rotation_degrees(session, cx, cy, start_angle, x, y, shift)
                };
                let mut model = self.model.borrow_mut();
                if let Some(session) = model.session.as_mut() {
                    session.preview = Some(Preview::Rotate {
                        index,
                        degrees_cw: degrees,
                    });
                    session.visual = session.visual.wrapping_add(1);
                }
                drop(model);
                self.canvas.queue_draw();
            }
            DragKind::Move {
                indices,
                bounds,
                targets,
            } => {
                let mut model = self.model.borrow_mut();
                if let Some(session) = model.session.as_mut() {
                    let origin = (drag.origin_x, drag.origin_y);
                    let (dx, dy, lines) =
                        snapped_move(session, origin, bounds, &targets, x, y, snap);
                    session.snap_lines = lines;
                    session.preview = Some(Preview::Move { indices, dx, dy });
                    session.visual = session.visual.wrapping_add(1);
                }
                drop(model);
                self.canvas.queue_draw();
            }
            DragKind::Guide {
                vertical,
                from,
                targets,
            } => {
                if let Some(session) = self.model.borrow_mut().session.as_mut() {
                    let draft = guide_draft(session, view, vertical, from, &targets, x, y, snap);
                    session.guide_draft = Some(draft);
                }
                self.canvas.queue_draw();
            }
            DragKind::Shape { ax, ay, targets } => {
                let mut model = self.model.borrow_mut();
                let style = model.shape_style;
                if let Some(session) = model.session.as_mut() {
                    let (draft, lines) =
                        shape_draft(session, style, &targets, (ax, ay), x, y, shift, snap);
                    session.shape_draft = draft;
                    session.snap_lines = lines;
                }
                drop(model);
                self.canvas.queue_draw();
            }
            DragKind::Select { ax, ay } => {
                let travelled = (x - drag.origin_x).hypot(y - drag.origin_y);
                let mut model = self.model.borrow_mut();
                let Some(session) = model.session.as_mut() else {
                    return;
                };
                if session.marquee.is_none() && travelled < CLICK_SLOP {
                    return;
                }
                let (bx, by) = widget_to_doc(session, x, y);
                session.marquee = Some((ax, ay, bx, by));
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

    fn end_drag(self: &Rc<Self>, x: f64, y: f64, shift: bool, ctrl: bool) {
        let Some(drag) = self.drag.borrow_mut().take() else {
            return;
        };
        let view = self.view();
        let snap = view.snap && !ctrl;
        if let Some(session) = self.model.borrow_mut().session.as_mut() {
            session.snap_lines.clear();
        }
        match drag.kind {
            DragKind::Move {
                indices,
                bounds,
                targets,
            } => {
                let (dx, dy) = {
                    let mut model = self.model.borrow_mut();
                    let Some(session) = model.session.as_mut() else {
                        return;
                    };
                    session.preview = None;
                    let origin = (drag.origin_x, drag.origin_y);
                    let (dx, dy, _) = snapped_move(session, origin, bounds, &targets, x, y, snap);
                    (dx, dy)
                };
                self.edit(Command::MoveLayers { indices, dx, dy });
            }
            DragKind::Shape { ax, ay, targets } => {
                let draft = {
                    let mut model = self.model.borrow_mut();
                    let style = model.shape_style;
                    let Some(session) = model.session.as_mut() else {
                        return;
                    };
                    session.shape_draft = None;
                    shape_draft(session, style, &targets, (ax, ay), x, y, shift, snap).0
                };
                match draft {
                    Some(draft) => self.edit(Command::AddShape {
                        spec: draft.spec,
                        x: draft.x as i32,
                        y: draft.y as i32,
                    }),
                    None => self.canvas.queue_draw(),
                }
            }
            DragKind::Guide {
                vertical,
                from,
                targets,
            } => {
                let guides = {
                    let mut model = self.model.borrow_mut();
                    let Some(session) = model.session.as_mut() else {
                        return;
                    };
                    session.guide_draft = None;
                    let draft = guide_draft(session, view, vertical, from, &targets, x, y, snap);
                    let mut guides = session.editor.document().guides().clone();
                    let list = if vertical {
                        &mut guides.x
                    } else {
                        &mut guides.y
                    };
                    if let Some(index) = from {
                        list.remove(index);
                    }
                    let position = draft.position.round() as i32;
                    if !draft.removing && !list.contains(&position) {
                        list.push(position);
                    }
                    guides
                };
                self.edit(Command::SetGuides { guides });
            }
            DragKind::Select { ax, ay } => {
                if let Some(session) = self.model.borrow_mut().session.as_mut() {
                    select_on_release(session, ax, ay, x, y, shift);
                }
                self.refresh();
            }
            DragKind::Resize {
                index,
                handle,
                origin,
                targets,
            } => {
                let rect = {
                    let model = self.model.borrow();
                    let Some(session) = model.session.as_ref() else {
                        return;
                    };
                    snapped_resize(session, origin, handle, &targets, x, y, shift, snap).0
                };
                if let Some(session) = self.model.borrow_mut().session.as_mut() {
                    session.preview = None;
                }
                self.edit(Command::ScaleLayer {
                    index,
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: rect.height,
                    filter: ScaleFilter::Lanczos3,
                });
            }
            DragKind::Rotate {
                index,
                cx,
                cy,
                start_angle,
            } => {
                let degrees = {
                    let model = self.model.borrow();
                    let Some(session) = model.session.as_ref() else {
                        return;
                    };
                    rotation_degrees(session, cx, cy, start_angle, x, y, shift)
                };
                if let Some(session) = self.model.borrow_mut().session.as_mut() {
                    session.preview = None;
                }
                self.edit(Command::RotateLayer {
                    index,
                    degrees_cw: degrees,
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
        model.chrome = colors.chrome_rgb();
        model.chrome_text = colors.chrome_text_rgb();
        drop(model);
        shell_for_css.canvas.queue_draw();
    };
    apply();
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_resource_path("/app/pixel/Pixel/icons");
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
                model.chrome = colors.chrome_rgb();
                model.chrome_text = colors.chrome_text_rgb();
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
    gtk::Box,
    gtk::ListBox,
) {
    let page = libadwaita::StatusPage::new();
    page.set_title("Pixel");
    page.set_description(Some(
        "Set up a canvas, stack layers, and export a flat image. Drop or paste an image to start.",
    ));
    let new_canvas = gtk::Button::with_label("New canvas");
    let open_image = gtk::Button::with_label("Open image");
    let open_project = gtk::Button::with_label("Open project");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::Center);
    row.append(&new_canvas);
    row.append(&open_image);
    row.append(&open_project);

    let recent_list = gtk::ListBox::new();
    recent_list.add_css_class("boxed-list");
    recent_list.set_selection_mode(gtk::SelectionMode::None);
    let heading = gtk::Label::new(Some("Recent"));
    heading.add_css_class("heading");
    heading.set_xalign(0.0);
    let recent_group = gtk::Box::new(gtk::Orientation::Vertical, 8);
    recent_group.append(&heading);
    recent_group.append(&recent_list);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 32);
    content.append(&row);
    content.append(&recent_group);
    let clamp = libadwaita::Clamp::new();
    clamp.set_maximum_size(480);
    clamp.set_child(Some(&content));
    page.set_child(Some(&clamp));
    (
        page,
        new_canvas,
        open_image,
        open_project,
        recent_group,
        recent_list,
    )
}

/// How many recent files the File menu lists.
const RECENT_LIMIT: usize = 10;

/// How many of them the welcome screen shows.
const WELCOME_RECENT: usize = 6;

/// Record a file Pixel opened or saved in the desktop's recent files.
fn remember(path: &Path) {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if let Ok(uri) = glib::filename_to_uri(&path, None) {
        gtk::RecentManager::default().add_item(&uri);
    }
}

/// Files Pixel opened or saved that still exist, newest first.
fn recent_files() -> Vec<PathBuf> {
    let Some(app) = glib::application_name() else {
        return Vec::new();
    };
    let mut items: Vec<_> = gtk::RecentManager::default()
        .items()
        .into_iter()
        .filter(|info| info.has_application(&app) && info.exists())
        .collect();
    items.sort_by_key(|info| std::cmp::Reverse(info.modified().to_unix()));
    items
        .iter()
        .filter_map(|info| gio::File::for_uri(&info.uri()).path())
        .take(RECENT_LIMIT)
        .collect()
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Menu labels treat `_` as a mnemonic marker, so double it to show one.
fn menu_label(text: &str) -> String {
    text.replace('_', "__")
}

/// A welcome-screen row: the file name over the folder it is in.
fn recent_row(path: &Path) -> libadwaita::ActionRow {
    let row = libadwaita::ActionRow::new();
    row.set_use_markup(false);
    row.set_title(&display_name(path));
    let folder = path.parent().map(tilde_path).unwrap_or_default();
    row.set_subtitle(&folder);
    row.set_activatable(true);
    row.set_tooltip_text(Some(&path.to_string_lossy()));
    let icon = if is_project(path) {
        "x-office-drawing-symbolic"
    } else {
        "image-x-generic-symbolic"
    };
    row.add_prefix(&gtk::Image::from_icon_name(icon));
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    row
}

/// A path with the home folder written as `~`.
fn tilde_path(path: &Path) -> String {
    match path.strip_prefix(glib::home_dir()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.to_string_lossy()),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

fn app_menu(recent: &gio::Menu) -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append_submenu(Some("File"), &{
        let menu = gio::Menu::new();
        menu.append_item(&menu_item("New Canvas", "new"));
        menu.append_item(&menu_item("Open Image", "open"));
        menu.append_item(&menu_item("Open Project", "open-project"));
        menu.append_submenu(Some("Open Recent"), recent);
        menu.append_item(&menu_item("Save", "save"));
        menu.append_item(&menu_item("Save As", "save-as"));
        menu.append_item(&menu_item("Export…", "export"));
        menu.append_item(&menu_item("Close", "close"));
        menu.append_item(&menu_item("Quit", "quit"));
        menu
    });
    menu.append_submenu(Some("Edit"), &{
        let menu = gio::Menu::new();
        menu.append_item(&menu_item("Undo", "undo"));
        menu.append_item(&menu_item("Redo", "redo"));
        menu.append_section(None, &{
            let menu = gio::Menu::new();
            menu.append_item(&menu_item("Cut", "cut"));
            menu.append_item(&menu_item("Copy", "copy"));
            menu.append_item(&menu_item("Copy Image", "copy-image"));
            menu.append_item(&menu_item("Paste", "paste"));
            menu
        });
        menu
    });
    menu.append_submenu(Some("Image"), &{
        let menu = gio::Menu::new();
        menu.append_item(&menu_item("Canvas Size…", "canvas-size"));
        menu.append_item(&menu_item("Image Size…", "image-size"));
        menu.append_item(&menu_item("Trim to Content", "trim"));
        menu.append_item(&menu_item("Rotate 90° Clockwise", "rotate-cw"));
        menu.append_item(&menu_item("Rotate 90° Counterclockwise", "rotate-ccw"));
        menu.append_item(&menu_item("Rotate 180°", "rotate-180"));
        menu.append_item(&menu_item("Flip Horizontal", "flip-h"));
        menu.append_item(&menu_item("Flip Vertical", "flip-v"));
        menu
    });
    menu.append_submenu(Some("Layer"), &{
        let menu = gio::Menu::new();
        menu.append_item(&menu_item("Add Layer", "add-layer"));
        menu.append_item(&menu_item("Add Image as Layer…", "place"));
        menu.append_item(&menu_item("Duplicate", "duplicate"));
        menu.append_item(&menu_item("Delete", "delete-layer"));
        menu.append_item(&menu_item("Edit Text…", "edit-text"));
        menu.append_item(&menu_item("Rasterize", "rasterize"));
        menu.append_item(&menu_item("Revert Adjustments", "revert-adjustments"));
        menu.append_item(&menu_item("Rotate…", "rotate-layer"));
        menu.append_item(&menu_item("Flip Horizontal", "flip-layer-h"));
        menu.append_item(&menu_item("Flip Vertical", "flip-layer-v"));
        menu.append_item(&menu_item("Group", "group"));
        menu.append_item(&menu_item("Ungroup", "ungroup"));
        menu.append_item(&menu_item("Raise", "raise"));
        menu.append_item(&menu_item("Lower", "lower"));
        menu
    });
    menu.append_submenu(Some("Adjust"), &{
        let menu = gio::Menu::new();
        let section = |names: &[&str]| {
            let section = gio::Menu::new();
            for name in names {
                let (_, label) = ADJUSTMENTS
                    .iter()
                    .find(|(action, _)| action == name)
                    .expect("every listed adjustment has a label");
                section.append_item(&menu_item(label, name));
            }
            section
        };
        menu.append_section(
            None,
            &section(&[
                "brightness-contrast",
                "hue-saturation",
                "levels",
                "grayscale",
            ]),
        );
        menu.append_section(None, &section(&["blur", "sharpen"]));
        menu.append_section(None, &{
            let section = gio::Menu::new();
            section.append_item(&menu_item("Revert Adjustments", "revert-adjustments"));
            section
        });
        menu
    });
    menu.append_submenu(Some("Selection"), &{
        let menu = gio::Menu::new();
        menu.append_section(None, &{
            let menu = gio::Menu::new();
            menu.append_item(&menu_item("Select All", "select-all"));
            menu.append_item(&menu_item("Deselect", "deselect"));
            menu
        });
        menu.append_item(&icon_row(
            "Align",
            &[
                ("Align Top Edges", "align-top"),
                ("Align Vertical Centers", "align-vertical-center"),
                ("Align Bottom Edges", "align-bottom"),
                ("Align Left Edges", "align-left"),
                ("Align Horizontal Centers", "align-horizontal-center"),
                ("Align Right Edges", "align-right"),
            ],
        ));
        menu.append_item(&icon_row(
            "Distribute",
            &[
                ("Distribute Horizontally", "distribute-horizontal"),
                ("Distribute Vertically", "distribute-vertical"),
            ],
        ));
        menu
    });
    menu.append_submenu(Some("View"), &{
        let menu = gio::Menu::new();
        menu.append_item(&menu_item("Zoom In", "zoom-in"));
        menu.append_item(&menu_item("Zoom Out", "zoom-out"));
        menu.append_item(&menu_item("Fit", "fit"));
        menu.append_item(&menu_item("Actual Size", "actual"));
        menu.append_section(None, &{
            let menu = gio::Menu::new();
            menu.append_item(&menu_item("Show Rulers", "show-rulers"));
            menu.append_item(&menu_item("Show Guides", "show-guides"));
            menu.append_item(&menu_item("Snap", "snap"));
            menu.append_item(&menu_item("Clear Guides", "clear-guides"));
            menu
        });
        menu.append_section(None, &{
            let menu = gio::Menu::new();
            menu.append_item(&menu_item("Select Tool", "tool-select"));
            menu.append_item(&menu_item("Move Tool", "tool-move"));
            menu.append_item(&menu_item("Crop Tool", "tool-crop"));
            menu.append_item(&menu_item("Text Tool", "tool-text"));
            menu.append_item(&menu_item("Shape Tool", "tool-shape"));
            menu
        });
        menu
    });
    menu
}

/// Shortcuts that must not beat a focused text field to the key: single keys,
/// and the clipboard and selection keys a text field uses too. The first
/// alternative is the one menus show.
const EDITING_SHORTCUTS: [(&str, &str); 14] = [
    ("tool-select", "v"),
    ("tool-move", "m"),
    ("tool-crop", "c"),
    ("tool-text", "t"),
    ("tool-shape", "u"),
    ("cut", "<Control>x"),
    ("copy", "<Control>c"),
    ("copy-image", "<Control><Shift>c"),
    ("paste", "<Control>v"),
    ("select-all", "<Control>a"),
    ("deselect", "<Control><Shift>a"),
    ("delete-layer", "Delete|BackSpace"),
    ("zoom-in", "plus|equal|KP_Add|<Control>plus|<Control>equal"),
    ("zoom-out", "minus|KP_Subtract|<Control>minus"),
];

/// How much one zoom in or out step scales the view.
const ZOOM_STEP: f64 = 1.25;

/// A menu item for a window action. Items for [`EDITING_SHORTCUTS`] show the
/// shortcut themselves, since menus only know application accelerators.
fn menu_item(label: &str, action: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), Some(&format!("win.{action}")));
    if let Some((_, keys)) = EDITING_SHORTCUTS.iter().find(|(name, _)| *name == action) {
        let shown = keys.split('|').next().unwrap_or(keys);
        item.set_attribute_value("accel", Some(&shown.to_variant()));
    }
    item
}

fn is_project(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("pixel")
}

/// A texture's pixels as straight RGBA.
fn texture_image(texture: &gdk::Texture) -> image::RgbaImage {
    let mut downloader = gdk::TextureDownloader::new(texture);
    downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = downloader.download_bytes();
    let (width, height) = (texture.width() as u32, texture.height() as u32);
    let row = width as usize * 4;
    let mut pixels = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        pixels.extend_from_slice(&bytes[y * stride..y * stride + row]);
    }
    image::RgbaImage::from_raw(width, height, pixels).expect("each row is width × 4 bytes")
}

/// How an adjustment reads in menus and tooltips, with its settings.
pub fn adjustment_label(adjustment: &Adjustment) -> String {
    let signed = |v: f32| format!("{:+.0}", v).replace('-', "−");
    match *adjustment {
        Adjustment::BrightnessContrast {
            brightness,
            contrast,
        } => format!(
            "Brightness and Contrast ({}, {})",
            signed(brightness),
            signed(contrast)
        ),
        Adjustment::HueSaturation {
            hue,
            saturation,
            lightness,
        } => format!(
            "Hue and Saturation ({}°, {}, {})",
            signed(hue),
            signed(saturation),
            signed(lightness)
        ),
        Adjustment::Levels {
            black,
            white,
            gamma,
        } => format!("Levels ({black}–{white}, {gamma:.2})"),
        Adjustment::Grayscale => "Grayscale".into(),
        Adjustment::Blur { radius } => format!("Blur ({radius:.1} px)"),
        Adjustment::Sharpen { amount, radius } => {
            format!("Sharpen ({amount:.0}%, {radius:.1} px)")
        }
    }
}

/// Adjustment and filter actions, with their menu labels. Each adds to the
/// one selected layer's list of adjustments, which can be taken off again.
const ADJUSTMENTS: [(&str, &str); 6] = [
    ("brightness-contrast", "Brightness and Contrast…"),
    ("hue-saturation", "Hue and Saturation…"),
    ("levels", "Levels…"),
    ("grayscale", "Grayscale"),
    ("blur", "Blur…"),
    ("sharpen", "Sharpen…"),
];

/// The sliders an adjustment's dialog shows, and how their values become the
/// adjustment. `None` for one that applies straight away.
fn adjustment_form(name: &str) -> Option<(Vec<dialogs::Slider>, fn(&[f64]) -> Adjustment)> {
    let slider = |label, min, max, value, digits| dialogs::Slider {
        label,
        min,
        max,
        value,
        digits,
    };
    let form: (Vec<dialogs::Slider>, fn(&[f64]) -> Adjustment) = match name {
        "brightness-contrast" => (
            vec![
                slider("Brightness", -100.0, 100.0, 0.0, 0),
                slider("Contrast", -100.0, 100.0, 0.0, 0),
            ],
            |v| Adjustment::BrightnessContrast {
                brightness: v[0] as f32,
                contrast: v[1] as f32,
            },
        ),
        "hue-saturation" => (
            vec![
                slider("Hue", -180.0, 180.0, 0.0, 0),
                slider("Saturation", -100.0, 100.0, 0.0, 0),
                slider("Lightness", -100.0, 100.0, 0.0, 0),
            ],
            |v| Adjustment::HueSaturation {
                hue: v[0] as f32,
                saturation: v[1] as f32,
                lightness: v[2] as f32,
            },
        ),
        "levels" => (
            vec![
                slider("Black point", 0.0, 254.0, 0.0, 0),
                slider("White point", 1.0, 255.0, 255.0, 0),
                slider("Midtones", 0.1, 5.0, 1.0, 2),
            ],
            |v| Adjustment::Levels {
                black: v[0].round() as u8,
                white: v[1].round() as u8,
                gamma: v[2] as f32,
            },
        ),
        "blur" => (vec![slider("Radius", 0.0, 50.0, 2.0, 1)], |v| {
            Adjustment::Blur {
                radius: v[0] as f32,
            }
        }),
        "sharpen" => (
            vec![
                slider("Amount", 0.0, 500.0, 100.0, 0),
                slider("Radius", 0.1, 10.0, 1.0, 1),
            ],
            |v| Adjustment::Sharpen {
                amount: v[0] as f32,
                radius: v[1] as f32,
            },
        ),
        _ => return None,
    };
    Some(form)
}

/// Align actions. A single layer aligns to the canvas.
const ALIGNMENTS: [(&str, Alignment); 6] = [
    ("align-left", Alignment::Left),
    ("align-horizontal-center", Alignment::HorizontalCenter),
    ("align-right", Alignment::Right),
    ("align-top", Alignment::Top),
    ("align-vertical-center", Alignment::VerticalCenter),
    ("align-bottom", Alignment::Bottom),
];

/// Distribute actions. They need three movable layers.
const DISTRIBUTIONS: [(&str, Axis); 2] = [
    ("distribute-horizontal", Axis::Horizontal),
    ("distribute-vertical", Axis::Vertical),
];

/// A titled menu section drawn as one row of icon buttons. Each action's icon
/// is `pixel-<action>-symbolic`, and its label is the tooltip.
fn icon_row(title: &str, items: &[(&str, &str)]) -> gio::MenuItem {
    let row = gio::Menu::new();
    for (label, action) in items {
        let item = gio::MenuItem::new(Some(label), Some(&format!("win.{action}")));
        item.set_attribute_value(
            "verb-icon",
            Some(&format!("pixel-{action}-symbolic").to_variant()),
        );
        item.set_attribute_value("tooltip", Some(&label.to_variant()));
        row.append_item(&item);
    }
    let section = gio::MenuItem::new_section(Some(title), &row);
    section.set_attribute_value("display-hint", Some(&"horizontal-buttons".to_variant()));
    section
}

/// Tool hint text. It wraps so it never holds the canvas pane open wider than
/// the splitter puts it.
fn hint_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_hexpand(true);
    label
}

/// A toolbar button with the `pixel-tool-<icon>-symbolic` icon over its label.
fn tool_button(label: &str, icon: &str, key: &str) -> gtk::ToggleButton {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let image = gtk::Image::from_icon_name(&format!("pixel-tool-{icon}-symbolic"));
    image.set_pixel_size(20);
    content.append(&image);
    content.append(&gtk::Label::new(Some(label)));
    let button = gtk::ToggleButton::new();
    button.set_child(Some(&content));
    button.set_tooltip_text(Some(&format!("{label} ({key})")));
    button.add_css_class("pixel-tool");
    button
}

fn clear_box(box_: &gtk::Box) {
    while let Some(child) = box_.first_child() {
        box_.remove(&child);
    }
}

fn rotation_degrees(
    session: &super::model::Session,
    cx: f64,
    cy: f64,
    start_angle: f64,
    x: f64,
    y: f64,
    shift: bool,
) -> f32 {
    let (px, py) = widget_to_doc(session, x, y);
    let degrees = clockwise_delta(start_angle, pointer_angle(cx, cy, px, py));
    if shift {
        snap_angle(degrees, 45.0)
    } else {
        degrees
    }
}

/// How far, in screen pixels, a select-tool press can wander and still count
/// as a click.
const CLICK_SLOP: f64 = 3.0;

/// Selected layers that a move can shift. Locked ones stay put.
fn movable_selection(doc: &Document) -> Vec<usize> {
    doc.selected_indices()
        .into_iter()
        .filter(|&index| !doc.layers()[index].locked)
        .collect()
}

/// Whole document pixels between a drag's start and `(x, y)`.
fn drag_offset(session: &super::model::Session, origin: (f64, f64), x: f64, y: f64) -> (i32, i32) {
    let (cx, cy) = widget_to_doc(session, x, y);
    let (ox, oy) = widget_to_doc(session, origin.0, origin.1);
    ((cx - ox).round() as i32, (cy - oy).round() as i32)
}

/// Finish a select-tool press. A drag box selects the layers it encloses. A
/// click selects the layer under it, or clears the selection over nothing.
/// Shift adds to the selection, and a Shift-click on a selected layer removes it.
fn select_on_release(
    session: &mut super::model::Session,
    ax: f64,
    ay: f64,
    x: f64,
    y: f64,
    shift: bool,
) {
    let (bx, by) = widget_to_doc(session, x, y);
    let doc = session.editor.document();
    let result = match session.marquee.take() {
        Some(_) => {
            let left = ax.min(bx).floor();
            let top = ay.min(by).floor();
            let area = PixelRect {
                x: left as i32,
                y: top as i32,
                width: (ax.max(bx).ceil() - left) as u32,
                height: (ay.max(by).ceil() - top) as u32,
            };
            let mut indices = doc.layers_within(area);
            if shift {
                indices.extend(doc.selected_indices());
                indices.sort_unstable();
                indices.dedup();
            }
            session.editor.set_selection(&indices)
        }
        None => match doc.layer_at(ax, ay) {
            Some(index) if shift => session.editor.toggle_selected(index),
            Some(index) => session.editor.set_active(index),
            None if shift => Ok(()),
            None => {
                session.editor.deselect();
                Ok(())
            }
        },
    };
    debug_assert!(result.is_ok(), "picked layers come from the document");
}

/// Update the selection for a move-tool press. The unlocked layer under the
/// pointer is selected alone unless it is already part of the selection.
/// Locked layers can't move, so the press looks through them. The handles and
/// the boxes of selected layers keep the selection, so a layer with
/// transparent areas can still be grabbed. A press on nothing deselects.
/// Returns whether the selection changed.
fn select_under_pointer(session: &mut super::model::Session, x: f64, y: f64) -> bool {
    if hit_at(session, x, y).is_some() {
        return false;
    }
    let (dx, dy) = widget_to_doc(session, x, y);
    let doc = session.editor.document();
    match doc.unlocked_layer_at(dx, dy) {
        // A layer inside a selected group moves with it.
        Some(index) if selected_or_inside_selection(doc, index) => false,
        Some(index) => session.editor.set_active(index).is_ok(),
        None if pointer_in_selection(session, x, y) => false,
        None if doc.selected_indices().is_empty() => false,
        None => {
            session.editor.deselect();
            true
        }
    }
}

/// The drag the move tool starts, or `None` when nothing movable is selected.
/// Handles only exist when one layer is selected.
fn move_drag(session: &super::model::Session, x: f64, y: f64, guides: bool) -> Option<DragKind> {
    let doc = session.editor.document();
    let Some(index) = doc.active_index() else {
        let indices = movable_selection(doc);
        if indices.is_empty() {
            return None;
        }
        // Everything inside a moving group moves with it.
        let indices = doc.with_contents(&indices);
        return Some(DragKind::Move {
            bounds: moving_bounds(doc, &indices),
            targets: SnapTargets::collect(doc, &indices, guides),
            indices,
        });
    };
    let targets = SnapTargets::collect(doc, &[index], guides);
    let layer = &doc.layers()[index];
    if layer.locked {
        return None;
    }
    let bounds = active_bounds(session)?;
    let kind = match hit_at(session, x, y) {
        Some(Handle::Rotate) => {
            let cx = bounds.x as f64 + bounds.width as f64 / 2.0;
            let cy = bounds.y as f64 + bounds.height as f64 / 2.0;
            let (px, py) = widget_to_doc(session, x, y);
            DragKind::Rotate {
                index,
                cx,
                cy,
                start_angle: pointer_angle(cx, cy, px, py),
            }
        }
        Some(handle) => DragKind::Resize {
            index,
            handle,
            origin: bounds,
            targets,
        },
        None => {
            let indices = doc.with_contents(&[index]);
            DragKind::Move {
                bounds: moving_bounds(doc, &indices),
                targets: SnapTargets::collect(doc, &indices, guides),
                indices,
            }
        }
    };
    Some(kind)
}

/// Whether the pointer is inside the box of a selected layer that can move.
fn pointer_in_selection(session: &super::model::Session, x: f64, y: f64) -> bool {
    movable_selection(session.editor.document())
        .into_iter()
        .any(|index| pointer_in_bounds(session, layer_bounds(session, index), x, y))
}

/// Whether the pointer is inside the box of a selected, locked layer.
fn pointer_on_locked_selection(session: &super::model::Session, x: f64, y: f64) -> bool {
    let doc = session.editor.document();
    doc.selected_indices().into_iter().any(|index| {
        doc.layers()[index].locked && pointer_in_bounds(session, layer_bounds(session, index), x, y)
    })
}

fn pointer_in_bounds(session: &super::model::Session, bounds: PixelRect, x: f64, y: f64) -> bool {
    let (left, top) = doc_to_widget(session, bounds.x as f64, bounds.y as f64);
    let (right, bottom) = doc_to_widget(
        session,
        bounds.x as f64 + bounds.width as f64,
        bounds.y as f64 + bounds.height as f64,
    );
    x >= left.min(right) && x <= left.max(right) && y >= top.min(bottom) && y <= top.max(bottom)
}

fn pointer_on_layer(session: &super::model::Session, x: f64, y: f64) -> bool {
    let (dx, dy) = widget_to_doc(session, x, y);
    session
        .editor
        .document()
        .unlocked_layer_at(dx, dy)
        .is_some()
}

/// A locked layer has no handles, so this is `None` for one.
fn hit_at(session: &super::model::Session, x: f64, y: f64) -> Option<Handle> {
    let layer = session.editor.document().active_layer()?;
    // Groups move but don't resize or rotate.
    if layer.locked || matches!(layer.kind, LayerKind::Group { .. }) {
        return None;
    }
    let bounds = active_bounds(session)?;
    let (left, top) = doc_to_widget(session, bounds.x as f64, bounds.y as f64);
    let (right, bottom) = doc_to_widget(
        session,
        bounds.x as f64 + bounds.width as f64,
        bounds.y as f64 + bounds.height as f64,
    );
    hit_handle(x, y, left, top, right, bottom, HANDLE_RADIUS, ROTATE_OFFSET)
}

fn hover_cursor(
    session: &super::model::Session,
    view: View,
    x: f64,
    y: f64,
) -> Option<&'static str> {
    if view.rulers && (x < RULER_SIZE || y < RULER_SIZE) {
        return Some("default");
    }
    if !session.space_down {
        if let Some((vertical, Some(_))) = guide_pickup(session, view, x, y) {
            return Some(if vertical { "ew-resize" } else { "ns-resize" });
        }
    }
    let name = match session.tool {
        Tool::Select if session.space_down => "grab",
        Tool::Select => "default",
        Tool::Crop | Tool::Shape => "crosshair",
        Tool::Text => "text",
        Tool::Move => match hit_at(session, x, y) {
            Some(Handle::Rotate) => "crosshair",
            Some(Handle::North | Handle::South) => "ns-resize",
            Some(Handle::East | Handle::West) => "ew-resize",
            Some(Handle::NorthWest | Handle::SouthEast) => "nwse-resize",
            Some(Handle::NorthEast | Handle::SouthWest) => "nesw-resize",
            None if session.space_down
                || pointer_on_layer(session, x, y)
                || pointer_in_selection(session, x, y) =>
            {
                "grab"
            }
            None if pointer_on_locked_selection(session, x, y) => "not-allowed",
            None => "default",
        },
    };
    Some(name)
}

/// Whether the layer, or a group it is in, is selected.
fn selected_or_inside_selection(doc: &Document, index: usize) -> bool {
    let mut at = Some(index);
    while let Some(index) = at {
        if doc.is_selected(index) {
            return true;
        }
        at = doc.parent_index(index);
    }
    false
}

/// The default color for text and shapes: a red that stands out on most
/// screenshots.
const ANNOTATION_RED: [u8; 4] = [229, 72, 77, 255];

/// A point pulled onto nearby lines when `snap` is on, with the lines it
/// snapped to.
fn snap_point(
    session: &super::model::Session,
    targets: &SnapTargets,
    x: f64,
    y: f64,
    snap: bool,
) -> ((f64, f64), Vec<SnapLine>) {
    let mut lines = Vec::new();
    if !snap {
        return ((x, y), lines);
    }
    let reach = SNAP_DISTANCE / session.zoom;
    let x = match snap_lines(&[x], &targets.x, reach) {
        Some((_, line)) => {
            lines.push(SnapLine::X(line));
            line
        }
        None => x,
    };
    let y = match snap_lines(&[y], &targets.y, reach) {
        Some((_, line)) => {
            lines.push(SnapLine::Y(line));
            line
        }
        None => y,
    };
    ((x, y), lines)
}

/// The shape a drag from `start` to `(x, y)` draws. Shift makes squares and
/// circles, and turns lines in steps of 45°. `None` while it is too small to
/// keep.
#[allow(clippy::too_many_arguments)]
fn shape_draft(
    session: &super::model::Session,
    style: ShapeStyle,
    targets: &SnapTargets,
    start: (f64, f64),
    x: f64,
    y: f64,
    shift: bool,
    snap: bool,
) -> (Option<ShapeDraft>, Vec<SnapLine>) {
    let (px, py) = widget_to_doc(session, x, y);
    let ((px, py), lines) = snap_point(session, targets, px, py, snap && !shift);
    let (mut dx, mut dy) = (px - start.0, py - start.1);
    if shift {
        match style.kind {
            ShapeKind::Rectangle | ShapeKind::Ellipse => {
                let side = dx.abs().max(dy.abs());
                dx = side.copysign(dx);
                dy = side.copysign(dy);
            }
            ShapeKind::Line | ShapeKind::Arrow => {
                let step = std::f64::consts::FRAC_PI_4;
                let angle = (dy.atan2(dx) / step).round() * step;
                let length = dx.hypot(dy);
                dx = length * angle.cos();
                dy = length * angle.sin();
            }
        }
    }
    let (dx, dy) = (dx.round(), dy.round());
    if dx.abs() < 2.0 && dy.abs() < 2.0 {
        return (None, lines);
    }
    let draft = ShapeDraft {
        spec: style.spec(dx as f32, dy as f32),
        x: (start.0 + dx.min(0.0)).round(),
        y: (start.1 + dy.min(0.0)).round(),
    };
    (Some(draft), lines)
}

/// The view settings that shape a drag or a hover.
#[derive(Clone, Copy)]
struct View {
    rulers: bool,
    guides: bool,
    snap: bool,
    width: f64,
    height: f64,
}

impl View {
    /// Space the rulers take along the top and left of the canvas.
    fn inset(self) -> f64 {
        if self.rulers {
            RULER_SIZE
        } else {
            0.0
        }
    }
}

/// How close, in screen pixels, the pointer has to be to grab a guide.
const GUIDE_REACH: f64 = 4.0;

/// What a press at `(x, y)` does with guides: pull a new one out of a ruler,
/// with `None`, or pick up an existing one by its place in its list. Handles
/// win over guides, and only the select and move tools pick them up.
fn guide_pickup(
    session: &super::model::Session,
    view: View,
    x: f64,
    y: f64,
) -> Option<(bool, Option<usize>)> {
    if view.rulers {
        match (x < RULER_SIZE, y < RULER_SIZE) {
            (true, true) => return None,
            (true, false) => return Some((true, None)),
            (false, true) => return Some((false, None)),
            (false, false) => {}
        }
    }
    if !view.guides || !matches!(session.tool, Tool::Select | Tool::Move) {
        return None;
    }
    if session.tool == Tool::Move && hit_at(session, x, y).is_some() {
        return None;
    }
    let guides = session.editor.document().guides();
    let nearest = |lines: &[i32], pointer: f64, vertical: bool| {
        lines
            .iter()
            .enumerate()
            .map(|(index, &line)| {
                let (wx, wy) = doc_to_widget(session, line as f64, line as f64);
                let at = if vertical { wx } else { wy };
                ((at - pointer).abs(), index)
            })
            .filter(|&(distance, _)| distance <= GUIDE_REACH)
            .min_by(|a, b| a.0.total_cmp(&b.0))
    };
    let vertical = nearest(&guides.x, x, true);
    let horizontal = nearest(&guides.y, y, false);
    match (vertical, horizontal) {
        (Some(v), Some(h)) if h.0 < v.0 => Some((false, Some(h.1))),
        (Some(v), _) => Some((true, Some(v.1))),
        (None, Some(h)) => Some((false, Some(h.1))),
        (None, None) => None,
    }
}

/// Where a guide being dragged to `(x, y)` would land. It snaps to canvas and
/// layer lines, and is marked for removal over its own ruler or off the canvas.
#[allow(clippy::too_many_arguments)]
fn guide_draft(
    session: &super::model::Session,
    view: View,
    vertical: bool,
    from: Option<usize>,
    targets: &SnapTargets,
    x: f64,
    y: f64,
    snap: bool,
) -> GuideDraft {
    let (px, py) = widget_to_doc(session, x, y);
    let (raw, lines) = if vertical {
        (px, &targets.x)
    } else {
        (py, &targets.y)
    };
    let snapped = snap
        .then(|| snap_lines(&[raw], lines, SNAP_DISTANCE / session.zoom))
        .flatten();
    let position = snapped.map_or(raw, |(_, target)| target).round();
    let over_ruler = view.rulers
        && if vertical {
            x < RULER_SIZE
        } else {
            y < RULER_SIZE
        };
    let outside = x < 0.0 || y < 0.0 || x > view.width || y > view.height;
    GuideDraft {
        vertical,
        position,
        from,
        removing: over_ruler || outside,
    }
}

/// A move drag's offset, with the moving box's edges or centre pulled onto a
/// nearby line when `snap` is on, and the lines it snapped to.
fn snapped_move(
    session: &super::model::Session,
    origin: (f64, f64),
    bounds: Option<PixelRect>,
    targets: &SnapTargets,
    x: f64,
    y: f64,
    snap: bool,
) -> (i32, i32, Vec<SnapLine>) {
    let (mut dx, mut dy) = drag_offset(session, origin, x, y);
    let mut lines = Vec::new();
    let Some(bounds) = bounds.filter(|_| snap) else {
        return (dx, dy, lines);
    };
    let reach = SNAP_DISTANCE / session.zoom;
    let moved = PixelRect {
        x: bounds.x + dx,
        y: bounds.y + dy,
        ..bounds
    };
    if let Some((shift, line)) = snap_lines(&x_lines(moved), &targets.x, reach) {
        dx += shift.round() as i32;
        lines.push(SnapLine::X(line));
    }
    if let Some((shift, line)) = snap_lines(&y_lines(moved), &targets.y, reach) {
        dy += shift.round() as i32;
        lines.push(SnapLine::Y(line));
    }
    (dx, dy, lines)
}

/// A resize drag's new box, with the dragged edges pulled onto nearby lines
/// when `snap` is on, and the lines they snapped to.
#[allow(clippy::too_many_arguments)]
fn snapped_resize(
    session: &super::model::Session,
    origin: PixelRect,
    handle: Handle,
    targets: &SnapTargets,
    x: f64,
    y: f64,
    keep_aspect: bool,
    snap: bool,
) -> (PixelRect, Vec<SnapLine>) {
    let (mut px, mut py) = widget_to_doc(session, x, y);
    let mut lines = Vec::new();
    if snap {
        let reach = SNAP_DISTANCE / session.zoom;
        let moves_x = !matches!(handle, Handle::North | Handle::South);
        let moves_y = !matches!(handle, Handle::East | Handle::West);
        if moves_x {
            if let Some((shift, line)) = snap_lines(&[px], &targets.x, reach) {
                px += shift;
                lines.push(SnapLine::X(line));
            }
        }
        if moves_y {
            if let Some((shift, line)) = snap_lines(&[py], &targets.y, reach) {
                py += shift;
                lines.push(SnapLine::Y(line));
            }
        }
    }
    (resize_rect(origin, handle, px, py, keep_aspect), lines)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_survives_the_trip_through_a_texture() {
        let mut image = image::RgbaImage::new(3, 2);
        image.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        image.put_pixel(2, 1, image::Rgba([10, 200, 30, 128]));
        assert_eq!(texture_image(upload(&image).upcast_ref()), image);
    }
}

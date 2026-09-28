//! Pan, zoom, and draw the flattened document. Input is reported upward.

use super::model::{active_bounds, doc_to_widget, rendered, Model, Session, Tool};
use gtk::gdk;
use gtk::glib;
use gtk::graphene;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use pixel::document::{rotate_handle_point, ROTATE_OFFSET};
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

pub enum CanvasInput {
    DragBegin { x: f64, y: f64, button: u32 },
    DragUpdate { x: f64, y: f64, shift: bool },
    DragEnd { x: f64, y: f64, shift: bool },
    Motion { x: f64, y: f64 },
    Leave,
    Scroll { x: f64, y: f64, dy: f64 },
    Resize { width: i32, height: i32 },
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Canvas {
        pub model: RefCell<Option<Rc<RefCell<Model>>>>,
        pub handler: RefCell<Option<Rc<dyn Fn(CanvasInput)>>>,
        pub image: RefCell<Option<(u64, gdk::MemoryTexture)>>,
        pub checker: OnceCell<gdk::MemoryTexture>,
        pub drag_origin: Cell<Option<(f64, f64)>>,
        pub pointer: Cell<Option<(f64, f64)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Canvas {
        const NAME: &'static str = "PixelCanvas";
        type Type = super::Canvas;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Canvas {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            obj.set_focusable(true);
            obj.set_can_focus(true);
            // Widgets don't clip their own drawing. A panned document would
            // otherwise paint over the splitter and the layers panel.
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.add_css_class("pixel-canvas");
            self.install_input();
        }
    }

    impl WidgetImpl for Canvas {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let width = self.obj().width() as f32;
            let height = self.obj().height() as f32;
            if width <= 0.0 || height <= 0.0 {
                return;
            }
            let Some(model) = self.model.borrow().clone() else {
                return;
            };
            let model = model.borrow();
            let (br, bg, bb) = model.background;
            snapshot.append_color(
                &gdk::RGBA::new(br, bg, bb, 1.0),
                &graphene::Rect::new(0.0, 0.0, width, height),
            );
            let Some(session) = model.session.as_ref() else {
                return;
            };
            self.paint_document(snapshot, session, &model);
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.emit(CanvasInput::Resize { width, height });
        }
    }

    impl Canvas {
        fn emit(&self, event: CanvasInput) {
            let handler = self.handler.borrow().clone();
            if let Some(handler) = handler {
                handler(event);
            }
        }

        fn install_input(&self) {
            let obj = self.obj();
            let drag = gtk::GestureDrag::new();
            drag.set_button(0);
            drag.set_exclusive(true);
            // Cell::clone copies the value into a new cell. All three handlers
            // have to share the widget's cell or the drag never updates.
            let canvas = obj.clone();
            drag.connect_drag_begin(move |gesture, x, y| {
                canvas.imp().drag_origin.set(Some((x, y)));
                let button = drag_button(gesture);
                canvas.imp().emit(CanvasInput::DragBegin { x, y, button });
            });
            let canvas = obj.clone();
            drag.connect_drag_update(move |gesture, dx, dy| {
                let Some((x0, y0)) = canvas.imp().drag_origin.get() else {
                    return;
                };
                let shift = gesture
                    .current_event_state()
                    .contains(gdk::ModifierType::SHIFT_MASK);
                canvas.imp().emit(CanvasInput::DragUpdate {
                    x: x0 + dx,
                    y: y0 + dy,
                    shift,
                });
            });
            let canvas = obj.clone();
            drag.connect_drag_end(move |gesture, dx, dy| {
                let Some((x0, y0)) = canvas.imp().drag_origin.take() else {
                    return;
                };
                let shift = gesture
                    .current_event_state()
                    .contains(gdk::ModifierType::SHIFT_MASK);
                canvas.imp().emit(CanvasInput::DragEnd {
                    x: x0 + dx,
                    y: y0 + dy,
                    shift,
                });
            });
            obj.add_controller(drag);

            let motion = gtk::EventControllerMotion::new();
            let canvas = obj.clone();
            motion.connect_motion(move |_, x, y| {
                canvas.imp().pointer.set(Some((x, y)));
                canvas.imp().emit(CanvasInput::Motion { x, y });
            });
            let canvas = obj.clone();
            motion.connect_leave(move |_| {
                canvas.imp().pointer.set(None);
                canvas.imp().emit(CanvasInput::Leave);
            });
            obj.add_controller(motion);

            let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
            let canvas = obj.clone();
            scroll.connect_scroll(move |_, _, dy| {
                let Some((x, y)) = canvas.imp().pointer.get() else {
                    return glib::Propagation::Proceed;
                };
                canvas.imp().emit(CanvasInput::Scroll { x, y, dy });
                glib::Propagation::Stop
            });
            obj.add_controller(scroll);
        }

        fn paint_document(&self, snapshot: &gtk::Snapshot, session: &Session, model: &Model) {
            let doc = session.editor.document();
            let texture = self.document_texture(session);
            let checker = self.checker_tile();
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                session.pan_x as f32,
                session.pan_y as f32,
            ));
            snapshot.scale(session.zoom as f32, session.zoom as f32);
            let bounds = graphene::Rect::new(0.0, 0.0, doc.width as f32, doc.height as f32);
            snapshot.push_repeat(&bounds, None);
            snapshot.append_texture(&checker, &graphene::Rect::new(0.0, 0.0, 32.0, 32.0));
            snapshot.pop();
            snapshot.append_texture(&texture, &bounds);
            if let Some(crop) = session.crop {
                let (r, g, b) = model.accent;
                snapshot.append_color(
                    &gdk::RGBA::new(r, g, b, 0.28),
                    &graphene::Rect::new(
                        crop.x as f32,
                        crop.y as f32,
                        crop.width as f32,
                        crop.height as f32,
                    ),
                );
                let border = (1.0 / session.zoom as f32).max(0.5);
                let color = gdk::RGBA::new(r, g, b, 1.0);
                let x = crop.x as f32;
                let y = crop.y as f32;
                let w = crop.width as f32;
                let h = crop.height as f32;
                snapshot.append_color(&color, &graphene::Rect::new(x, y, w, border));
                snapshot.append_color(&color, &graphene::Rect::new(x, y + h - border, w, border));
                snapshot.append_color(&color, &graphene::Rect::new(x, y, border, h));
                snapshot.append_color(&color, &graphene::Rect::new(x + w - border, y, border, h));
            }
            snapshot.restore();
            if session.tool == Tool::Move && !doc.active_layer().locked {
                draw_handles(snapshot, session, model.accent);
            }
        }

        fn document_texture(&self, session: &Session) -> gdk::MemoryTexture {
            if let Some((visual, texture)) = self.image.borrow().as_ref() {
                if *visual == session.visual {
                    return texture.clone();
                }
            }
            let image = rendered(session);
            let texture = upload(&image);
            *self.image.borrow_mut() = Some((session.visual, texture.clone()));
            texture
        }

        fn checker_tile(&self) -> gdk::MemoryTexture {
            self.checker
                .get_or_init(|| {
                    let mut pixels = vec![0u8; 32 * 32 * 4];
                    for y in 0..32 {
                        for x in 0..32 {
                            let light = ((x / 16) + (y / 16)) % 2 == 0;
                            let color: [u8; 4] = if light {
                                [196, 196, 196, 255]
                            } else {
                                [148, 148, 148, 255]
                            };
                            let index = (y * 32 + x) * 4;
                            pixels[index..index + 4].copy_from_slice(&color);
                        }
                    }
                    upload_raw(32, 32, pixels)
                })
                .clone()
        }
    }
}

glib::wrapper! {
    pub struct Canvas(ObjectSubclass<imp::Canvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Canvas {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn set_model(&self, model: Rc<RefCell<Model>>) {
        *self.imp().model.borrow_mut() = Some(model);
    }

    pub fn set_handler(&self, handler: Rc<dyn Fn(CanvasInput)>) {
        *self.imp().handler.borrow_mut() = Some(handler);
    }
}

fn draw_handles(snapshot: &gtk::Snapshot, session: &Session, accent: (f32, f32, f32)) {
    let bounds = active_bounds(session);
    let (left, top) = doc_to_widget(session, bounds.x as f64, bounds.y as f64);
    let (right, bottom) = doc_to_widget(
        session,
        bounds.x as f64 + bounds.width as f64,
        bounds.y as f64 + bounds.height as f64,
    );
    let (r, g, b) = accent;
    let stroke = gdk::RGBA::new(r, g, b, 1.0);
    let fill = gdk::RGBA::new(1.0, 1.0, 1.0, 1.0);
    stroke_rect(snapshot, left, top, right, bottom, &stroke);

    let mid_x = (left + right) / 2.0;
    let mid_y = (top + bottom) / 2.0;
    let (rotate_x, rotate_y) = rotate_handle_point(left, top, right, bottom, ROTATE_OFFSET);
    let (stem_y, stem_h) = if rotate_y < top {
        (rotate_y, top - rotate_y)
    } else {
        (bottom, rotate_y - bottom)
    };
    snapshot.append_color(
        &stroke,
        &graphene::Rect::new(mid_x as f32 - 1.0, stem_y as f32, 2.0, stem_h as f32),
    );
    draw_knob(snapshot, rotate_x, rotate_y, &fill, &stroke);
    for (x, y) in [
        (left, top),
        (mid_x, top),
        (right, top),
        (right, mid_y),
        (right, bottom),
        (mid_x, bottom),
        (left, bottom),
        (left, mid_y),
    ] {
        draw_knob(snapshot, x, y, &fill, &stroke);
    }
}

fn stroke_rect(
    snapshot: &gtk::Snapshot,
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
    color: &gdk::RGBA,
) {
    let width = (right - left).abs() as f32;
    let height = (bottom - top).abs() as f32;
    let x = left.min(right) as f32;
    let y = top.min(bottom) as f32;
    snapshot.append_color(color, &graphene::Rect::new(x, y, width, 1.0));
    snapshot.append_color(color, &graphene::Rect::new(x, y + height - 1.0, width, 1.0));
    snapshot.append_color(color, &graphene::Rect::new(x, y, 1.0, height));
    snapshot.append_color(color, &graphene::Rect::new(x + width - 1.0, y, 1.0, height));
}

fn draw_knob(snapshot: &gtk::Snapshot, cx: f64, cy: f64, fill: &gdk::RGBA, stroke: &gdk::RGBA) {
    let size = 11.0_f32;
    let x = (cx as f32) - size / 2.0;
    let y = (cy as f32) - size / 2.0;
    snapshot.append_color(stroke, &graphene::Rect::new(x, y, size, size));
    snapshot.append_color(
        fill,
        &graphene::Rect::new(x + 1.5, y + 1.5, size - 3.0, size - 3.0),
    );
}

fn drag_button(gesture: &gtk::GestureDrag) -> u32 {
    let button = gesture.current_button();
    if button != 0 {
        return button;
    }
    let state = gesture.current_event_state();
    if state.contains(gdk::ModifierType::BUTTON2_MASK) {
        gdk::BUTTON_MIDDLE
    } else if state.contains(gdk::ModifierType::BUTTON3_MASK) {
        gdk::BUTTON_SECONDARY
    } else {
        gdk::BUTTON_PRIMARY
    }
}

fn upload(image: &image::RgbaImage) -> gdk::MemoryTexture {
    upload_raw(image.width(), image.height(), image.as_raw().to_vec())
}

fn upload_raw(width: u32, height: u32, pixels: Vec<u8>) -> gdk::MemoryTexture {
    let stride = width as usize * 4;
    let bytes = glib::Bytes::from_owned(pixels);
    gdk::MemoryTexture::new(
        width as i32,
        height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &bytes,
        stride,
    )
}

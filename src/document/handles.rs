//! Geometry for the move tool's resize and rotate handles.

use super::MAX_EDGE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    NorthWest,
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    Rotate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub const HANDLE_RADIUS: f64 = 7.0;
pub const ROTATE_OFFSET: f64 = 28.0;

/// Which handle, if any, contains a point in the same space as `left/top/right/bottom`.
/// `rotate_offset` is how far above the top edge the rotate handle sits.
pub fn hit_handle(
    px: f64,
    py: f64,
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
    radius: f64,
    rotate_offset: f64,
) -> Option<Handle> {
    let (rx, ry) = rotate_handle_point(left, top, right, bottom, rotate_offset);
    let points = [
        (Handle::Rotate, rx, ry),
        (Handle::NorthWest, left, top),
        (Handle::NorthEast, right, top),
        (Handle::SouthEast, right, bottom),
        (Handle::SouthWest, left, bottom),
        (Handle::North, (left + right) / 2.0, top),
        (Handle::East, right, (top + bottom) / 2.0),
        (Handle::South, (left + right) / 2.0, bottom),
        (Handle::West, left, (top + bottom) / 2.0),
    ];
    points.into_iter().find_map(|(handle, x, y)| {
        let dx = px - x;
        let dy = py - y;
        (dx * dx + dy * dy <= radius * radius).then_some(handle)
    })
}

/// Screen position of the rotate handle. It sits above the layer, or below it
/// when the top edge is too close to the top of the view.
pub fn rotate_handle_point(
    left: f64,
    top: f64,
    right: f64,
    bottom: f64,
    offset: f64,
) -> (f64, f64) {
    let x = (left + right) / 2.0;
    let y = if top >= offset {
        top - offset
    } else {
        bottom + offset
    };
    (x, y)
}

/// Resize `start` by dragging `handle` to `(px, py)` in document pixels.
/// The opposite side stays put. `lock_aspect` keeps the starting ratio on corners.
pub fn resize_rect(
    start: PixelRect,
    handle: Handle,
    px: f64,
    py: f64,
    lock_aspect: bool,
) -> PixelRect {
    let left0 = start.x as f64;
    let top0 = start.y as f64;
    let right0 = left0 + start.width as f64;
    let bottom0 = top0 + start.height as f64;
    let aspect = start.width as f64 / start.height.max(1) as f64;

    let (moves_left, moves_top, moves_right, moves_bottom) = match handle {
        Handle::NorthWest => (true, true, false, false),
        Handle::North => (false, true, false, false),
        Handle::NorthEast => (false, true, true, false),
        Handle::East => (false, false, true, false),
        Handle::SouthEast => (false, false, true, true),
        Handle::South => (false, false, false, true),
        Handle::SouthWest => (true, false, false, true),
        Handle::West => (true, false, false, false),
        Handle::Rotate => return start,
    };

    let corner = moves_left != moves_right && moves_top != moves_bottom;
    if lock_aspect && corner {
        let (ax, ay) = if moves_right && moves_bottom {
            (left0, top0)
        } else if moves_left && moves_bottom {
            (right0, top0)
        } else if moves_right && moves_top {
            (left0, bottom0)
        } else {
            (right0, bottom0)
        };
        let sign_x = if px >= ax { 1.0 } else { -1.0 };
        let sign_y = if py >= ay { 1.0 } else { -1.0 };
        let w_pointer = (px - ax).abs().max(1.0);
        let h_pointer = (py - ay).abs().max(1.0);
        let (w, h) = if w_pointer / aspect >= h_pointer {
            (w_pointer, (w_pointer / aspect).max(1.0))
        } else {
            ((h_pointer * aspect).max(1.0), h_pointer)
        };
        return rect_from_anchor(ax, ay, sign_x, sign_y, w, h);
    }

    let mut left = left0;
    let mut right = right0;
    let mut top = top0;
    let mut bottom = bottom0;
    if moves_left {
        left = px;
    }
    if moves_right {
        right = px;
    }
    if moves_top {
        top = py;
    }
    if moves_bottom {
        bottom = py;
    }
    if left > right {
        std::mem::swap(&mut left, &mut right);
    }
    if top > bottom {
        std::mem::swap(&mut top, &mut bottom);
    }
    PixelRect {
        x: left.round() as i32,
        y: top.round() as i32,
        width: clamp_edge((right - left).round()),
        height: clamp_edge((bottom - top).round()),
    }
}

fn rect_from_anchor(ax: f64, ay: f64, sign_x: f64, sign_y: f64, w: f64, h: f64) -> PixelRect {
    let w = clamp_edge(w.round()) as f64;
    let h = clamp_edge(h.round()) as f64;
    let x = if sign_x > 0.0 { ax } else { ax - w };
    let y = if sign_y > 0.0 { ay } else { ay - h };
    PixelRect {
        x: x.round() as i32,
        y: y.round() as i32,
        width: w as u32,
        height: h as u32,
    }
}

fn clamp_edge(value: f64) -> u32 {
    value.clamp(1.0, MAX_EDGE as f64) as u32
}

/// Clockwise angle of a document point around a center, in radians.
/// Document y grows downward, so the angle increases clockwise.
pub fn pointer_angle(cx: f64, cy: f64, px: f64, py: f64) -> f64 {
    (py - cy).atan2(px - cx)
}

/// Round `degrees` to the nearest multiple of `step`.
pub fn snap_angle(degrees: f32, step: f32) -> f32 {
    if !degrees.is_finite() || !step.is_finite() || step <= 0.0 {
        return degrees;
    }
    (degrees / step).round() * step
}

/// Shortest clockwise turn from `start` to `end`, in degrees.
pub fn clockwise_delta(start: f64, end: f64) -> f32 {
    let mut delta = end - start;
    let pi = std::f64::consts::PI;
    while delta > pi {
        delta -= 2.0 * pi;
    }
    while delta < -pi {
        delta += 2.0 * pi;
    }
    delta.to_degrees() as f32
}

/// Axis-aligned bounds after a clockwise rotation around the layer center.
pub fn rotated_bounds(x: i32, y: i32, width: u32, height: u32, degrees_cw: f32) -> PixelRect {
    let theta = degrees_cw.to_radians();
    let (sin, cos) = (theta.sin().abs(), theta.cos().abs());
    let width_f = width as f32;
    let height_f = height as f32;
    let new_w = (width_f * cos + height_f * sin).ceil().max(1.0) as u32;
    let new_h = (width_f * sin + height_f * cos).ceil().max(1.0) as u32;
    let cx = x as f64 + width as f64 / 2.0;
    let cy = y as f64 + height as f64 / 2.0;
    PixelRect {
        x: (cx - new_w as f64 / 2.0).round() as i32,
        y: (cy - new_h as f64 / 2.0).round() as i32,
        width: new_w,
        height: new_h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: u32, height: u32) -> PixelRect {
        PixelRect {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn east_handle_keeps_the_left_edge() {
        let resized = resize_rect(rect(10, 10, 20, 10), Handle::East, 50.0, 15.0, false);
        assert_eq!(resized, rect(10, 10, 40, 10));
    }

    #[test]
    fn west_handle_dragged_past_the_right_edge_flips() {
        let resized = resize_rect(rect(0, 0, 10, 10), Handle::West, 20.0, 4.0, false);
        assert_eq!(resized, rect(10, 0, 10, 10));
    }

    #[test]
    fn corner_aspect_lock_uses_the_further_axis() {
        let resized = resize_rect(rect(0, 0, 20, 10), Handle::SouthEast, 40.0, 40.0, true);
        assert_eq!(resized, rect(0, 0, 80, 40));
    }

    #[test]
    fn hit_prefers_a_corner_and_finds_the_rotate_handle() {
        assert_eq!(
            hit_handle(0.0, 0.0, 0.0, 0.0, 100.0, 40.0, 6.0, 24.0),
            Some(Handle::NorthWest)
        );
        assert_eq!(
            hit_handle(50.0, 64.0, 0.0, 0.0, 100.0, 40.0, 6.0, 24.0),
            Some(Handle::Rotate)
        );
        assert_eq!(
            hit_handle(50.0, 20.0, 0.0, 0.0, 100.0, 40.0, 6.0, 24.0),
            None
        );
    }

    #[test]
    #[test]
    fn shift_snap_lands_on_forty_five_degree_steps() {
        assert_eq!(snap_angle(50.0, 45.0), 45.0);
        assert_eq!(snap_angle(70.0, 45.0), 90.0);
        assert_eq!(snap_angle(-30.0, 45.0), -45.0);
        assert_eq!(snap_angle(10.0, 45.0), 0.0);
    }

    fn clockwise_delta_is_a_quarter_turn_downward() {
        let start = pointer_angle(0.0, 0.0, 10.0, 0.0);
        let end = pointer_angle(0.0, 0.0, 0.0, 10.0);
        let degrees = clockwise_delta(start, end);
        assert!((degrees - 90.0).abs() < 0.01, "{degrees}");
    }

    #[test]
    fn quarter_turn_swaps_a_wide_layer_bounds() {
        let bounds = rotated_bounds(0, 0, 8, 4, 90.0);
        assert!(bounds.height > bounds.width);
        assert!((4..=5).contains(&bounds.width));
        assert!((8..=9).contains(&bounds.height));
    }
}

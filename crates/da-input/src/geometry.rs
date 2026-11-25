//! Screen-edge maths for moving the pointer between computers.
//!
//! Everything works on the bounding box of all displays (in the OS's global
//! screen coordinates), which covers side-by-side and stacked layouts. A
//! position along an edge travels as a fraction (0..=65535), so screens of
//! different sizes line up proportionally.

use crate::wire::Edge;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Rect { x, y, w, h }
    }

    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// The smallest rectangle around all displays.
pub fn bounds(screens: &[Rect]) -> Option<Rect> {
    let first = screens.first()?;
    let (mut x0, mut y0) = (first.x, first.y);
    let (mut x1, mut y1) = (first.x + first.w, first.y + first.h);
    for r in &screens[1..] {
        x0 = x0.min(r.x);
        y0 = y0.min(r.y);
        x1 = x1.max(r.x + r.w);
        y1 = y1.max(r.y + r.h);
    }
    Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
}

fn fraction(v: f64, start: f64, len: f64) -> u16 {
    if len <= 1.0 {
        return 32_768;
    }
    (((v - start) / (len - 1.0)).clamp(0.0, 1.0) * 65535.0).round() as u16
}

/// Position along `edge` of a point, as 0..=65535.
fn pos_along(b: &Rect, edge: Edge, x: f64, y: f64) -> u16 {
    match edge {
        Edge::Left | Edge::Right => fraction(y, b.y, b.h),
        Edge::Top | Edge::Bottom => fraction(x, b.x, b.w),
    }
}

/// If the point is on the outer `edge` of the layout, its position along it.
pub fn at_edge(screens: &[Rect], edge: Edge, x: f64, y: f64) -> Option<u16> {
    let b = bounds(screens)?;
    let on = match edge {
        Edge::Left => x <= b.x + 0.5,
        Edge::Right => x >= b.x + b.w - 1.5,
        Edge::Top => y <= b.y + 0.5,
        Edge::Bottom => y >= b.y + b.h - 1.5,
    };
    on.then(|| pos_along(&b, edge, x, y))
}

/// If moving the pointer to (x, y) would take it past the outer `edge`, the
/// position along that edge where it left.
pub fn overshoot(screens: &[Rect], edge: Edge, x: f64, y: f64) -> Option<u16> {
    let b = bounds(screens)?;
    let past = match edge {
        Edge::Left => x < b.x,
        Edge::Right => x > b.x + b.w - 1.0,
        Edge::Top => y < b.y,
        Edge::Bottom => y > b.y + b.h - 1.0,
    };
    past.then(|| pos_along(&b, edge, x, y))
}

/// Where the pointer should appear when it comes in over `edge` at `pos`:
/// a couple of pixels inside, on whichever display is nearest.
pub fn entry_point(screens: &[Rect], edge: Edge, pos: u16) -> Option<(f64, f64)> {
    let b = bounds(screens)?;
    let f = f64::from(pos) / 65535.0;
    const INSET: f64 = 2.0;
    let (x, y) = match edge {
        Edge::Left => (b.x + INSET, b.y + f * (b.h - 1.0)),
        Edge::Right => (b.x + b.w - 1.0 - INSET, b.y + f * (b.h - 1.0)),
        Edge::Top => (b.x + f * (b.w - 1.0), b.y + INSET),
        Edge::Bottom => (b.x + f * (b.w - 1.0), b.y + b.h - 1.0 - INSET),
    };
    Some(snap(screens, x, y))
}

/// The nearest point on any display.
pub fn snap(screens: &[Rect], x: f64, y: f64) -> (f64, f64) {
    if screens.iter().any(|r| r.contains(x, y)) || screens.is_empty() {
        return (x, y);
    }
    let mut best = (x, y);
    let mut best_d = f64::MAX;
    for r in screens {
        let cx = x.clamp(r.x, r.x + r.w - 1.0);
        let cy = y.clamp(r.y, r.y + r.h - 1.0);
        let d = (cx - x).powi(2) + (cy - y).powi(2);
        if d < best_d {
            best_d = d;
            best = (cx, cy);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 1920x1080 main display with a 1280x1024 display to its left, lower.
    fn pc() -> Vec<Rect> {
        vec![
            Rect::new(0.0, 0.0, 1920.0, 1080.0),
            Rect::new(-1280.0, 200.0, 1280.0, 1024.0),
        ]
    }

    #[test]
    fn bounds_cover_all_displays() {
        assert_eq!(bounds(&pc()), Some(Rect::new(-1280.0, 0.0, 3200.0, 1224.0)));
    }

    #[test]
    fn detects_the_outer_edge_only() {
        let s = pc();
        assert!(at_edge(&s, Edge::Left, -1280.0, 600.0).is_some());
        assert!(at_edge(&s, Edge::Left, 0.0, 600.0).is_none(), "inner seam");
        assert!(at_edge(&s, Edge::Right, 1919.0, 10.0).is_some());
        assert!(at_edge(&s, Edge::Top, 500.0, 0.0).is_some());
    }

    #[test]
    fn overshoot_reports_position() {
        let s = vec![Rect::new(0.0, 0.0, 1000.0, 1001.0)];
        assert_eq!(overshoot(&s, Edge::Left, -3.0, 500.0), Some(32_768));
        assert_eq!(overshoot(&s, Edge::Left, 2.0, 500.0), None);
        assert_eq!(overshoot(&s, Edge::Bottom, 0.0, 1001.0), Some(0));
    }

    #[test]
    fn entry_lands_on_a_real_display() {
        let s = pc();
        // Top of the left edge: the left display starts lower, so snap down.
        let (x, y) = entry_point(&s, Edge::Left, 0).unwrap();
        assert_eq!((x, y), (-1278.0, 200.0));
        let (x, y) = entry_point(&s, Edge::Right, 65535).unwrap();
        assert_eq!(x, 1917.0);
        assert_eq!(y, 1079.0);
    }

    #[test]
    fn crossing_keeps_proportional_height() {
        let pc = vec![Rect::new(0.0, 0.0, 1920.0, 1081.0)];
        let mac = vec![Rect::new(0.0, 0.0, 2560.0, 1441.0)];
        let pos = at_edge(&pc, Edge::Left, 0.0, 270.0).unwrap();
        let (_, y) = entry_point(&mac, Edge::Right, pos).unwrap();
        assert!((y - 360.0).abs() < 1.0, "a quarter of the way down: {y}");
    }
}

use super::Backend;
use crate::geometry::Rect;
use crate::wire::MouseButton;
use anyhow::{bail, Result};

pub struct Platform;

impl Platform {
    pub fn new() -> Result<Self> {
        bail!("receiving keyboard and mouse input is not supported on this OS yet")
    }
}

impl Backend for Platform {
    fn key(&mut self, _: u16, _: bool) {}
    fn mouse_move(&mut self, _: i32, _: i32) {}
    fn button(&mut self, _: MouseButton, _: bool) {}
    fn wheel(&mut self, _: i32, _: i32) {}
    fn cursor(&self) -> (f64, f64) {
        (0.0, 0.0)
    }
    fn screens(&self) -> Vec<Rect> {
        Vec::new()
    }
    fn warp(&mut self, _: f64, _: f64) {}
}

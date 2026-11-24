use super::Backend;
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
}

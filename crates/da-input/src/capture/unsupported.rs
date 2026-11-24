use super::Shared;
use crate::switcher::Target;
use anyhow::{bail, Result};
use std::sync::Arc;

pub(super) fn start(_shared: Arc<Shared>) -> Result<()> {
    bail!("sharing this computer's keyboard and mouse is not supported on this OS yet")
}

pub(super) fn release_local_key(_usage: u16) {}

pub(super) fn on_switch(_t: Target, _held: Vec<u16>) {}

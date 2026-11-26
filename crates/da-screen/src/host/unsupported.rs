use super::{HostOptions, Sink};
use crate::DisplayInfo;
use anyhow::{bail, Result};

pub(super) struct Host;

impl Host {
    pub(super) fn start(_opts: HostOptions, _sink: Sink) -> Result<Self> {
        bail!("sharing this screen is not supported on this OS yet")
    }

    pub(super) fn request_keyframe(&self) {}

    pub(super) fn bounds(&self) -> Option<crate::DisplayBounds> {
        None
    }
}

pub(super) fn displays() -> Vec<DisplayInfo> {
    Vec::new()
}

pub(super) fn capture_permission(_prompt: bool) -> bool {
    false
}

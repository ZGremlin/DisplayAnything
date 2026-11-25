use crate::wire::EncodedFrame;
use anyhow::{bail, Result};
use raw_window_handle::RawWindowHandle;

pub(super) struct Presenter;

impl Presenter {
    pub(super) unsafe fn new(_window: RawWindowHandle) -> Result<Self> {
        bail!("showing a remote screen is not supported on this OS yet")
    }

    pub(super) fn push(&self, _frame: EncodedFrame) {}

    pub(super) fn take_keyframe_request(&self) -> bool {
        false
    }

    pub(super) fn set_video_size(&self, _width: u32, _height: u32) {}

    pub(super) fn video_size(&self) -> Option<(u32, u32)> {
        None
    }
}

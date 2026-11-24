//! Plain-text clipboard access (images come later).

pub fn get_text() -> Option<String> {
    arboard::Clipboard::new()
        .ok()?
        .get_text()
        .ok()
        .filter(|s| !s.is_empty())
}

pub fn set_text(text: &str) {
    match arboard::Clipboard::new() {
        Ok(mut c) => {
            if let Err(e) = c.set_text(text.to_owned()) {
                tracing::debug!("could not set clipboard: {e}");
            }
        }
        Err(e) => tracing::debug!("clipboard unavailable: {e}"),
    }
}

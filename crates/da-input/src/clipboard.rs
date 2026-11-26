//! Clipboard access for text, images and files. Images cross the wire as
//! PNG; files as their names and contents.

use crate::wire::{Frame, MAX_CLIPBOARD_IMAGE};
use anyhow::{anyhow, bail, Result};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Counts clipboard changes seen on this machine, from a local copy or from
/// the other computer. Logged so two machines' logs can be read side by side.
static CHANGES: AtomicU64 = AtomicU64::new(0);
/// Fingerprint of the clipboard as last read to send.
static LAST_READ: AtomicU64 = AtomicU64::new(0);
/// The current switch, counted per machine. Logged with each switch line.
static SWITCHES: AtomicU64 = AtomicU64::new(0);

/// Largest total size of files we send (100 MiB).
pub const MAX_FILES_SENT: usize = 100 << 20;
/// How many received batches to keep in the temporary folder.
const KEEP_RECEIVED: usize = 5;

/// What the clipboard holds, in a form that can cross the wire.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Clip {
    Text(String),
    /// A PNG-encoded image.
    Png(Vec<u8>),
    /// Files: (file name, contents).
    Files(Vec<(String, Vec<u8>)>),
}

impl Clip {
    pub fn is_empty(&self) -> bool {
        match self {
            Clip::Text(t) => t.is_empty(),
            Clip::Png(b) => b.is_empty(),
            Clip::Files(f) => f.is_empty(),
        }
    }

    pub fn into_frame(self) -> Frame {
        match self {
            Clip::Text(t) => Frame::ClipboardText(t),
            Clip::Png(b) => Frame::ClipboardImage(b),
            Clip::Files(f) => Frame::ClipboardFiles(f),
        }
    }
}

/// A short description of a clip for the log.
pub fn describe(clip: &Clip) -> String {
    match clip {
        Clip::Text(t) => format!("text ({} bytes)", t.len()),
        Clip::Png(b) => format!("image ({} bytes)", b.len()),
        Clip::Files(f) => format!(
            "{} file(s), {} bytes",
            f.len(),
            f.iter().map(|(_, d)| d.len()).sum::<usize>()
        ),
    }
}

/// A short fingerprint of a clip, so the same content can be matched across
/// two machines' logs.
pub fn fingerprint(clip: &Clip) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    clip.hash(&mut h);
    h.finish()
}

/// Starts a new switch and returns its number. Logged on this machine only.
pub fn begin_switch(direction: &str) -> u64 {
    let n = SWITCHES.fetch_add(1, Ordering::SeqCst) + 1;
    tracing::info!("switch {n}: {direction}");
    n
}

/// The number of the current switch on this machine.
pub fn current_switch() -> u64 {
    SWITCHES.load(Ordering::SeqCst)
}

/// This machine's clipboard to send: files if it has any, then text, then an
/// image. None if there is nothing we can send.
pub fn get() -> Option<Clip> {
    let clip = read_clipboard();
    match &clip {
        Some(c) => {
            let fp = fingerprint(c);
            log_change_if_new("local copy", fp, c);
            tracing::info!(
                "switch {}: read to send {} hash={fp:016x}",
                current_switch(),
                describe(c)
            );
        }
        None => tracing::info!("switch {}: read to send nothing", current_switch()),
    }
    clip
}

/// Logs a clipboard change when its content differs from the last one seen.
fn log_change_if_new(source: &str, fp: u64, clip: &Clip) {
    if LAST_READ.swap(fp, Ordering::SeqCst) != fp {
        let n = CHANGES.fetch_add(1, Ordering::SeqCst) + 1;
        tracing::info!(
            "clipboard change #{n}: {source}, {} hash={fp:016x}",
            describe(clip)
        );
    }
}

fn read_clipboard() -> Option<Clip> {
    if let Some(paths) = file_paths() {
        if let Some(first) = paths.first() {
            tracing::info!("pasteboard first file: {}", first.display());
        }
        // Files we received from the other computer are already on its
        // side; sending them back would only echo them.
        let received = std::env::temp_dir().join("DisplayAnything-received");
        if !paths.is_empty() && paths.iter().all(|p| p.starts_with(&received)) {
            tracing::info!("the file on the clipboard is one we received; not sent back");
            return None;
        }
        if !paths.is_empty() {
            if let Some(clip) = read_files(&paths) {
                return Some(clip);
            }
            // The copy may still be settling, or a cloud file may still be
            // downloading. Look once more shortly after before giving up.
            std::thread::sleep(std::time::Duration::from_millis(300));
            return read_files(&paths);
        }
    }
    let mut c = arboard::Clipboard::new().ok()?;
    if let Ok(t) = c.get_text() {
        if !t.is_empty() {
            return Some(Clip::Text(t));
        }
    }
    let img = c.get_image().ok()?;
    match encode_png(img.width, img.height, &img.bytes) {
        Ok(png) if png.len() <= MAX_CLIPBOARD_IMAGE => Some(Clip::Png(png)),
        Ok(png) => {
            tracing::info!(bytes = png.len(), "clipboard image is too large to send");
            None
        }
        Err(e) => {
            tracing::debug!("could not encode the clipboard image: {e:#}");
            None
        }
    }
}

/// Fingerprint of the last clipboard we received from, or sent to, the other
/// computer. Used so the same content is not sent back and forth, and so a
/// stale copy does not overwrite a newer one.
static LAST: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);

/// Whether this clip is the same content we last synced with the other
/// computer. Such a clip is not new, so it is not sent or set again.
pub fn is_synced(clip: &Clip) -> bool {
    LAST.lock()
        .map(|l| *l == Some(fingerprint(clip)))
        .unwrap_or(false)
}

/// Remember that this clip was synced, whichever direction it went.
pub fn mark_synced(clip: &Clip) {
    if let Ok(mut l) = LAST.lock() {
        *l = Some(fingerprint(clip));
    }
}

/// Put something received from the other computer on this clipboard.
pub fn set(clip: &Clip) {
    let fp = fingerprint(clip);
    let n = CHANGES.fetch_add(1, Ordering::SeqCst) + 1;
    LAST_READ.store(fp, Ordering::SeqCst);
    tracing::info!(
        "clipboard change #{n}: from the other computer, {} hash={fp:016x}",
        describe(clip)
    );
    mark_synced(clip);
    let mut c = match arboard::Clipboard::new() {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("clipboard unavailable: {e}");
            return;
        }
    };
    let result = match clip {
        Clip::Text(t) => c.set_text(t.clone()).map_err(anyhow::Error::from),
        Clip::Png(b) => decode_png(b).and_then(|img| c.set_image(img).map_err(anyhow::Error::from)),
        Clip::Files(files) => receive_files(files).and_then(|paths| set_file_paths(&paths)),
    };
    match result {
        Ok(()) => tracing::info!("clipboard set from the other computer: {}", describe(clip)),
        Err(e) => tracing::info!("could not set the clipboard: {e:#}"),
    }
}

fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width as u32, height as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        // Favour speed: this runs while the user is waiting for a switch.
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(out)
}

fn decode_png(bytes: &[u8]) -> Result<arboard::ImageData<'static>> {
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes)).read_info()?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| anyhow!("image is too large"))?;
    let mut buf = vec![0; size];
    let info = reader.next_frame(&mut buf)?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        bail!("unexpected image format");
    }
    buf.truncate(info.buffer_size());
    Ok(arboard::ImageData {
        width: info.width as usize,
        height: info.height as usize,
        bytes: Cow::Owned(buf),
    })
}

/// Read the files a copy put on the clipboard, if any.
fn read_files(paths: &[PathBuf]) -> Option<Clip> {
    let mut files = Vec::new();
    let mut total = 0usize;
    for p in paths {
        let meta = match std::fs::metadata(p) {
            Ok(m) => m,
            Err(e) => {
                tracing::info!(path = %p.display(), "could not stat copied file: {e}");
                continue;
            }
        };
        if !meta.is_file() {
            tracing::info!(path = %p.display(), "folders are not shared yet; skipping");
            continue;
        }
        total += meta.len() as usize;
        if total > MAX_FILES_SENT {
            tracing::info!(total, "files on the clipboard are too large to send");
            return None;
        }
        let data = match std::fs::read(p) {
            Ok(d) => d,
            Err(e) => {
                tracing::info!(path = %p.display(), "could not read copied file: {e}");
                continue;
            }
        };
        let Some(name) = p.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            tracing::info!(path = %p.display(), "copied file has no name; skipping");
            continue;
        };
        files.push((name, data));
    }
    (!files.is_empty()).then_some(Clip::Files(files))
}

/// Write received files to a fresh folder and return their paths, so they
/// can be put on the clipboard for pasting.
fn receive_files(files: &[(String, Vec<u8>)]) -> Result<Vec<PathBuf>> {
    let root = std::env::temp_dir().join("DisplayAnything-received");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = root.join(stamp.to_string());
    std::fs::create_dir_all(&dir)?;
    let mut out = Vec::with_capacity(files.len());
    for (i, (name, data)) in files.iter().enumerate() {
        let mut path = dir.join(safe_name(name));
        if path.exists() {
            path = dir.join(format!("{i}-{}", safe_name(name)));
        }
        std::fs::write(&path, data)?;
        out.push(path);
    }
    prune_received(&root);
    Ok(out)
}

/// A file name from the other computer, reduced to a plain name.
fn safe_name(name: &str) -> String {
    Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .unwrap_or("file")
        .to_string()
}

/// Remove all but the newest received batches.
fn prune_received(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut dirs: Vec<(u128, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let stamp = e.file_name().to_str()?.parse::<u128>().ok()?;
            Some((stamp, e.path()))
        })
        .collect();
    dirs.sort();
    let extra = dirs.len().saturating_sub(KEEP_RECEIVED);
    for (_, path) in dirs.into_iter().take(extra) {
        let _ = std::fs::remove_dir_all(path);
    }
}

#[cfg(windows)]
fn file_paths() -> Option<Vec<PathBuf>> {
    let names: Vec<String> = clipboard_win::get_clipboard(clipboard_win::formats::FileList).ok()?;
    Some(names.into_iter().map(PathBuf::from).collect())
}

/// Adds the "Preferred DropEffect" format with DROPEFFECT_COPY. The clipboard
/// must already be open and emptied by this thread.
#[cfg(windows)]
fn add_copy_drop_effect() -> Result<()> {
    use windows::core::w;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{RegisterClipboardFormatW, SetClipboardData};
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
    const DROPEFFECT_COPY: u32 = 1;
    unsafe {
        let format = RegisterClipboardFormatW(w!("Preferred DropEffect"));
        if format == 0 {
            bail!("could not register the drop effect format");
        }
        let mem = GlobalAlloc(GMEM_MOVEABLE, std::mem::size_of::<u32>())?;
        let ptr = GlobalLock(mem) as *mut u32;
        if ptr.is_null() {
            bail!("could not lock memory for the drop effect");
        }
        *ptr = DROPEFFECT_COPY;
        let _ = GlobalUnlock(mem);
        // The system owns the memory once SetClipboardData succeeds.
        SetClipboardData(format, HANDLE(mem.0 as _))?;
    }
    Ok(())
}

#[cfg(windows)]
fn set_file_paths(paths: &[PathBuf]) -> Result<()> {
    use clipboard_win::Setter;
    let names: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    // OLE keeps its own copy of the last clipboard object, such as Explorer's
    // copy. Paste and OLE readers can keep seeing that old object after a raw
    // write, so release it first.
    unsafe {
        let _ = windows::Win32::System::Ole::OleInitialize(None);
        let _ = windows::Win32::System::Ole::OleSetClipboard(
            None::<&windows::Win32::System::Com::IDataObject>,
        );
    }
    let result = {
        let _clip = clipboard_win::Clipboard::new_attempts(10).map_err(|e| anyhow!("{e}"))?;
        let written = clipboard_win::formats::FileList
            .write_clipboard(&names[..])
            .map_err(|e| anyhow!("{e}"));
        // Explorer also expects "Preferred DropEffect" (copy) next to the file
        // list, as it puts there on a copy. Without it Explorer may not offer
        // Paste.
        if written.is_ok() {
            if let Err(e) = add_copy_drop_effect() {
                tracing::info!("could not add the copy drop effect: {e:#}");
            }
        }
        written
    };
    // Read the clipboard back, now that it is closed, to confirm the file list
    // is really there and the files still exist.
    let exist = paths.iter().filter(|p| p.exists()).count();
    match clipboard_win::get_clipboard::<Vec<String>, _>(clipboard_win::formats::FileList) {
        Ok(back) => tracing::info!(
            written = names.len(),
            exist,
            read_back = back.len(),
            first = back.first().map(String::as_str).unwrap_or(""),
            "clipboard file list read back"
        ),
        Err(e) => tracing::info!(
            written = names.len(),
            exist,
            "clipboard file list read back failed: {e}"
        ),
    }
    result
}

// NSFilenamesPboardType is deprecated by Apple, but it is still the type
// Finder and the other apps use for copied files.
/// Files on the Mac's pasteboard. Finder puts each file on its own item as
/// a file URL; older apps use the file-name list, so both are read.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn file_paths() -> Option<Vec<PathBuf>> {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{NSFilenamesPboardType, NSPasteboard, NSPasteboardTypeFileURL};
    use objc2_foundation::{NSArray, NSString};
    let pb = NSPasteboard::generalPasteboard();
    let mut paths = Vec::new();
    if let Some(items) = pb.pasteboardItems() {
        for item in items.iter() {
            let Some(url) = (unsafe { item.stringForType(NSPasteboardTypeFileURL) }) else {
                continue;
            };
            let url = url.to_string();
            // Finder can give a file-reference URL (file:///.file/id=...)
            // instead of a path; ask Foundation for the real path.
            let path = file_url_to_path(&url)
                .filter(|p| !p.starts_with("/.file/"))
                .or_else(|| resolve_file_reference(&url));
            if let Some(p) = path {
                paths.push(p);
            }
        }
    }
    if paths.is_empty() {
        let plist: Retained<AnyObject> =
            pb.propertyListForType(unsafe { NSFilenamesPboardType })?;
        let list: Retained<NSArray> = plist.downcast().ok()?;
        paths = list
            .iter()
            .filter_map(|o| {
                o.downcast_ref::<NSString>()
                    .map(|s| PathBuf::from(s.to_string()))
            })
            .collect();
    }
    tracing::info!(count = paths.len(), "pasteboard file list read");
    Some(paths)
}

/// The real path of a file-reference URL, or None if Foundation can't resolve it.
#[cfg(target_os = "macos")]
fn resolve_file_reference(url: &str) -> Option<PathBuf> {
    use objc2_foundation::{NSString, NSURL};
    let url = NSURL::URLWithString(&NSString::from_str(url))?;
    let real = url.filePathURL()?;
    let path = real.path()?;
    Some(PathBuf::from(path.to_string()))
}

/// "file:///a/b%20c.txt" -> "/a/b c.txt"
#[cfg(any(target_os = "macos", test))]
fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok().map(PathBuf::from)
}

#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn set_file_paths(paths: &[PathBuf]) -> Result<()> {
    use objc2::rc::Retained;
    use objc2_app_kit::{NSFilenamesPboardType, NSPasteboard};
    use objc2_foundation::{NSArray, NSString};
    let items: Vec<Retained<NSString>> = paths
        .iter()
        .map(|p| NSString::from_str(&p.to_string_lossy()))
        .collect();
    let list = NSArray::from_retained_slice(&items);
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    let ok = unsafe { pb.setPropertyList_forType(&list, NSFilenamesPboardType) };
    if ok {
        // Read the pasteboard back with the same reader used for sending, to
        // confirm the files we just wrote are what a reply would send.
        let back = file_paths();
        tracing::info!(
            written = paths.len(),
            read_back = ?back.as_ref().map(|v| v.len()),
            first = ?back.as_ref().and_then(|v| v.first()),
            "pasteboard read back after set"
        );
        Ok(())
    } else {
        bail!("the pasteboard refused the files")
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn file_paths() -> Option<Vec<PathBuf>> {
    None
}

#[cfg(not(any(windows, target_os = "macos")))]
fn set_file_paths(_paths: &[PathBuf]) -> Result<()> {
    bail!("files on the clipboard are not supported on this system")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_become_paths() {
        assert_eq!(
            file_url_to_path("file:///Users/me/My%20File%C3%A9.txt"),
            Some(PathBuf::from("/Users/me/My Fileé.txt"))
        );
        assert_eq!(
            file_url_to_path("file://localhost/tmp/a.png"),
            Some(PathBuf::from("/tmp/a.png"))
        );
        assert_eq!(file_url_to_path("https://example.com/x"), None);
    }

    #[test]
    fn received_names_cannot_leave_their_folder() {
        assert_eq!(safe_name("../../evil.txt"), "evil.txt");
        assert_eq!(safe_name(".."), "file");
        assert_eq!(safe_name(""), "file");
        assert_eq!(safe_name("photo.jpg"), "photo.jpg");
    }

    #[test]
    fn png_round_trips_pixels() {
        let rgba: Vec<u8> = (0..2 * 3 * 4).map(|i| i as u8 * 7).collect();
        let png = encode_png(2, 3, &rgba).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        let img = decode_png(&png).unwrap();
        assert_eq!((img.width, img.height), (2, 3));
        assert_eq!(img.bytes.as_ref(), rgba.as_slice());
    }

    #[test]
    fn a_non_png_is_rejected() {
        assert!(decode_png(b"not an image").is_err());
    }
}

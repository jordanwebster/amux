//! The system clipboard, read for something to attach.
//!
//! Ctrl+V is the one path an image reaches a draft by: a terminal cannot
//! deliver image bytes through a bracketed paste, so the TUI asks the
//! platform clipboard directly. Everything here is I/O against the host,
//! kept behind [`ClipboardContent`] so the key handlers — and their tests
//! — deal only in the value.

use std::path::PathBuf;

/// What the clipboard held when Ctrl+V was pressed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClipboardContent {
    Image { mime: String, bytes: Vec<u8> },
    Path(PathBuf),
    Text(String),
    Empty,
}

/// Reads the clipboard, preferring an image over the text form.
///
/// A clipboard holding one line that names a readable file is a `Path`, so
/// copying a file in a file manager and pressing Ctrl+V attaches the file
/// rather than typing its name. Anything else is text, and an unavailable
/// clipboard is `Empty` — no clipboard is a normal state on a headless
/// host, never an error worth stating.
#[cfg(target_os = "macos")]
pub fn read_clipboard() -> ClipboardContent {
    let Some(read) = pasteboard::read() else {
        return ClipboardContent::Empty;
    };
    if let Some(bytes) = read.png {
        return ClipboardContent::Image {
            mime: "image/png".to_string(),
            bytes,
        };
    }
    match read.text {
        Some(text) => classify(text),
        None => ClipboardContent::Empty,
    }
}

/// Reads the clipboard, preferring an image over the text form; see the
/// macOS reader for what each answer means.
#[cfg(not(target_os = "macos"))]
pub fn read_clipboard() -> ClipboardContent {
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return ClipboardContent::Empty;
    };
    if let Ok(image) = clipboard.get_image()
        && let Some(bytes) = encode_png(&image)
    {
        return ClipboardContent::Image {
            mime: "image/png".to_string(),
            bytes,
        };
    }
    match clipboard.get_text() {
        Ok(text) => classify(text),
        Err(_) => ClipboardContent::Empty,
    }
}

/// Splits clipboard text into a file path or plain text.
pub(crate) fn classify(text: String) -> ClipboardContent {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return ClipboardContent::Empty;
    }
    if !trimmed.contains('\n') {
        let path = PathBuf::from(trimmed);
        // Only an absolute path: a relative one would resolve against the
        // TUI's working directory, which is not where the person copied it.
        if path.is_absolute() && path.is_file() {
            return ClipboardContent::Path(path);
        }
    }
    ClipboardContent::Text(text)
}

/// Encodes the clipboard's RGBA image as a PNG.
#[cfg(not(target_os = "macos"))]
fn encode_png(image: &arboard::ImageData<'_>) -> Option<Vec<u8>> {
    let width = u32::try_from(image.width).ok()?;
    let height = u32::try_from(image.height).ok()?;
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&image.bytes).ok()?;
    }
    Some(out)
}

/// macOS's general pasteboard, reached through the Objective-C runtime
/// loaded when it is first read.
///
/// Linking AppKit, as a clipboard crate does, makes the dynamic loader map
/// it into every process this binary starts, and every agent process is this
/// binary: AppKit and the frameworks under it cost each agent about half a
/// mebibyte of footprint and part of its launch, for a framework only Ctrl+V
/// in the terminal client uses. Opening them here puts that cost on the one
/// process that pastes, at the moment it pastes.
#[cfg(target_os = "macos")]
mod pasteboard {
    use std::ffi::{CStr, c_char, c_void};
    use std::sync::OnceLock;

    type Id = *mut c_void;
    type Sel = *mut c_void;

    /// `NSBitmapImageFileTypePNG`.
    const PNG_FILE_TYPE: usize = 4;

    pub struct Read {
        pub png: Option<Vec<u8>>,
        pub text: Option<String>,
    }

    struct Runtime {
        get_class: unsafe extern "C" fn(*const c_char) -> Id,
        selector: unsafe extern "C" fn(*const c_char) -> Sel,
        /// `objc_msgSend`, called through the exact signature of each
        /// message, as the runtime requires.
        send: *const c_void,
        pool_push: unsafe extern "C" fn() -> *mut c_void,
        pool_pop: unsafe extern "C" fn(*mut c_void),
    }

    // The runtime's entry points are process-wide functions.
    unsafe impl Send for Runtime {}
    unsafe impl Sync for Runtime {}

    fn runtime() -> Option<&'static Runtime> {
        static RUNTIME: OnceLock<Option<Runtime>> = OnceLock::new();
        RUNTIME.get_or_init(open).as_ref()
    }

    fn open() -> Option<Runtime> {
        // SAFETY: dlopen and dlsym take NUL-terminated names, and each symbol
        // is read as the signature the Objective-C runtime declares for it.
        unsafe {
            let flags = libc::RTLD_LAZY | libc::RTLD_LOCAL;
            let appkit = libc::dlopen(
                c"/System/Library/Frameworks/AppKit.framework/AppKit".as_ptr(),
                flags,
            );
            let objc = libc::dlopen(c"/usr/lib/libobjc.A.dylib".as_ptr(), flags);
            if appkit.is_null() || objc.is_null() {
                return None;
            }
            Some(Runtime {
                get_class: symbol(objc, c"objc_getClass")?,
                selector: symbol(objc, c"sel_registerName")?,
                send: symbol(objc, c"objc_msgSend")?,
                pool_push: symbol(objc, c"objc_autoreleasePoolPush")?,
                pool_pop: symbol(objc, c"objc_autoreleasePoolPop")?,
            })
        }
    }

    /// A symbol of `library` as `T`, which must be the symbol's own
    /// pointer type.
    unsafe fn symbol<T: Copy>(library: *mut c_void, name: &CStr) -> Option<T> {
        const { assert!(size_of::<T>() == size_of::<*mut c_void>()) };
        let found = unsafe { libc::dlsym(library, name.as_ptr()) };
        (!found.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, T>(&found) })
    }

    impl Runtime {
        unsafe fn class(&self, name: &CStr) -> Id {
            unsafe { (self.get_class)(name.as_ptr()) }
        }

        unsafe fn sel(&self, name: &CStr) -> Sel {
            unsafe { (self.selector)(name.as_ptr()) }
        }

        /// A message with no argument, answering an object or a pointer.
        unsafe fn send0<R>(&self, to: Id, name: &CStr) -> R {
            let send = unsafe {
                std::mem::transmute::<*const c_void, unsafe extern "C" fn(Id, Sel) -> R>(self.send)
            };
            unsafe { send(to, self.sel(name)) }
        }

        /// A message with one pointer argument, answering an object.
        unsafe fn send1(&self, to: Id, name: &CStr, argument: *const c_void) -> Id {
            let send = unsafe {
                std::mem::transmute::<
                    *const c_void,
                    unsafe extern "C" fn(Id, Sel, *const c_void) -> Id,
                >(self.send)
            };
            unsafe { send(to, self.sel(name), argument) }
        }

        unsafe fn string(&self, text: &CStr) -> Id {
            unsafe {
                self.send1(
                    self.class(c"NSString"),
                    c"stringWithUTF8String:",
                    text.as_ptr().cast(),
                )
            }
        }

        unsafe fn bytes(&self, data: Id) -> Vec<u8> {
            unsafe {
                let length: usize = self.send0(data, c"length");
                let bytes: *const u8 = self.send0(data, c"bytes");
                if length == 0 || bytes.is_null() {
                    return Vec::new();
                }
                std::slice::from_raw_parts(bytes, length).to_vec()
            }
        }

        /// The pasteboard's image as PNG: its own PNG when it has one, else
        /// its TIFF (what most applications copy an image as) re-encoded.
        unsafe fn png(&self, pasteboard: Id) -> Option<Vec<u8>> {
            unsafe {
                let data_for = |kind: &CStr| {
                    let data = self.send1(pasteboard, c"dataForType:", self.string(kind));
                    (!data.is_null()).then_some(data)
                };
                if let Some(png) = data_for(c"public.png") {
                    return Some(self.bytes(png));
                }
                let tiff = data_for(c"public.tiff")?;
                let image = self.send1(self.class(c"NSBitmapImageRep"), c"imageRepWithData:", tiff);
                if image.is_null() {
                    return None;
                }
                let properties: Id = self.send0(self.class(c"NSDictionary"), c"dictionary");
                let encode = std::mem::transmute::<
                    *const c_void,
                    unsafe extern "C" fn(Id, Sel, usize, Id) -> Id,
                >(self.send);
                let png = encode(
                    image,
                    self.sel(c"representationUsingType:properties:"),
                    PNG_FILE_TYPE,
                    properties,
                );
                (!png.is_null()).then(|| self.bytes(png))
            }
        }

        unsafe fn text(&self, pasteboard: Id) -> Option<String> {
            unsafe {
                let string = self.send1(
                    pasteboard,
                    c"stringForType:",
                    self.string(c"public.utf8-plain-text"),
                );
                if string.is_null() {
                    return None;
                }
                let utf8: *const c_char = self.send0(string, c"UTF8String");
                (!utf8.is_null()).then(|| CStr::from_ptr(utf8).to_string_lossy().into_owned())
            }
        }
    }

    /// The general pasteboard's image and text, or `None` when there is no
    /// pasteboard to read (no window server, or AppKit missing).
    pub fn read() -> Option<Read> {
        let runtime = runtime()?;
        // SAFETY: every message is sent to an object the runtime answered
        // (checked for nil) with the signature AppKit declares for it, inside
        // an autorelease pool that outlives every object read here.
        unsafe {
            let pool = (runtime.pool_push)();
            let class = runtime.class(c"NSPasteboard");
            let read = (!class.is_null())
                .then(|| runtime.send0::<Id>(class, c"generalPasteboard"))
                .filter(|pasteboard| !pasteboard.is_null())
                .map(|pasteboard| Read {
                    png: runtime.png(pasteboard),
                    text: runtime.text(pasteboard),
                });
            (runtime.pool_pop)(pool);
            read
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_text_naming_a_readable_file_is_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.md");
        std::fs::write(&file, b"notes").unwrap();
        assert_eq!(
            classify(format!("  {}\n", file.display())),
            ClipboardContent::Path(file)
        );
    }

    #[test]
    fn a_path_that_names_nothing_stays_text() {
        let text = "/no/such/file.md".to_string();
        assert_eq!(classify(text.clone()), ClipboardContent::Text(text));
    }

    #[test]
    fn empty_clipboard_text_is_empty() {
        assert_eq!(classify("  \n ".to_string()), ClipboardContent::Empty);
    }
}

//! iOS's side of files and sharing, through UIKit (objc2): the document
//! picker for opening and saving, the Photos picker for pictures, the share
//! sheet, and the app's Documents folder (shown in the Files app) for
//! exports. The same functions as `android.rs`, behind `crate::platform`.
//!
//! Every call comes from egui's frame, on the main thread; the pickers
//! answer through delegates into `PICKED`, which `picker_poll` drains.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use block2::RcBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{ClassType, DeclaredClass, class, declare_class, msg_send, msg_send_id, mutability};
use objc2_foundation::{
    CGPoint, CGRect, CGSize, MainThreadMarker, NSArray, NSError, NSString, NSURL,
};
use objc2_ui_kit::{
    UIActivityViewController, UIApplication, UIDocumentPickerDelegate,
    UIDocumentPickerViewController, UIViewController,
};
use objc2_uniform_type_identifiers::UTType;

// The Photos picker (PHPickerViewController) is in PhotosUI, which the
// objc2 0.2 bindings only cover for macOS: called by name instead.
#[link(name = "PhotosUI", kind = "framework")]
unsafe extern "C" {}

/// An export moved into the Documents folder: what to tell the user, and
/// what the share sheet offers.
#[derive(Clone, Debug)]
pub struct PublishedFile {
    pub message: String,
    pub share_uri: Option<String>,
    pub share_mime: Option<String>,
}

/// The paths a picker answered with (empty when cancelled), until polled.
static PICKED: Mutex<Option<Vec<String>>> = Mutex::new(None);

thread_local! {
    /// The pickers keep their delegates weakly: the open picker's is kept
    /// here.
    static DELEGATE: RefCell<Option<Retained<NSObject>>> = const { RefCell::new(None) };
}

fn answer(paths: Vec<String>) {
    if let Ok(mut picked) = PICKED.lock() {
        *picked = Some(paths);
    }
}

fn main_thread() -> Result<MainThreadMarker, String> {
    MainThreadMarker::new().ok_or_else(|| "UIKit called off the main thread".to_string())
}

/// The view controller on top: the one a picker is presented from.
// `windows` is deprecated for each scene's own list, but still lists them
// all, and the app has the one window.
#[allow(deprecated)]
fn top_controller(mtm: MainThreadMarker) -> Result<Retained<UIViewController>, String> {
    let app = UIApplication::sharedApplication(mtm);
    let mut top = app
        .windows()
        .iter()
        .find_map(|w| w.rootViewController())
        .ok_or("No window to show the picker over")?;
    // SAFETY: plain property reads on the main thread.
    while let Some(next) = unsafe { top.presentedViewController() } {
        top = next;
    }
    Ok(top)
}

fn present(mtm: MainThreadMarker, controller: &UIViewController) -> Result<(), String> {
    let top = top_controller(mtm)?;
    // SAFETY: main thread; no completion block.
    unsafe { top.presentViewController_animated_completion(controller, true, None) };
    Ok(())
}

/// The app's folder for files on their way out (exports before they're
/// published, saves before they're exported).
pub fn cache_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("rusty-painter");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// A path in `dir` for `name` that doesn't exist yet ("name 2.png", …).
fn unused_path(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    if !path.exists() {
        return path;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) => (stem, format!(".{ext}")),
        None => (name, String::new()),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} {n}{ext}")))
        .find(|p| !p.exists())
        .unwrap_or(path)
}

declare_class!(
    /// Answers the document picker (opening files).
    struct DocumentDelegate;

    unsafe impl ClassType for DocumentDelegate {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "RustyPainterDocumentDelegate";
    }

    impl DeclaredClass for DocumentDelegate {
        type Ivars = ();
    }

    unsafe impl NSObjectProtocol for DocumentDelegate {}

    unsafe impl UIDocumentPickerDelegate for DocumentDelegate {
        #[method(documentPicker:didPickDocumentsAtURLs:)]
        fn did_pick(&self, _picker: &UIDocumentPickerViewController, urls: &NSArray<NSURL>) {
            // Opened as copies: the files are the app's, in its tmp folder.
            // SAFETY: plain property reads.
            let paths = urls
                .iter()
                .filter_map(|url| unsafe { url.path() })
                .map(|p| p.to_string())
                .collect();
            answer(paths);
        }

        #[method(documentPickerWasCancelled:)]
        fn cancelled(&self, _picker: &UIDocumentPickerViewController) {
            answer(Vec::new());
        }
    }
);

impl DocumentDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(());
        // SAFETY: NSObject's `init`.
        unsafe { msg_send_id![super(this), init] }
    }
}

declare_class!(
    /// Answers the Photos picker: each picture copied out of Photos (as
    /// JPEG or PNG, which the image decoders read, rather than HEIC).
    struct PhotoDelegate;

    unsafe impl ClassType for PhotoDelegate {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "RustyPainterPhotoDelegate";
    }

    impl DeclaredClass for PhotoDelegate {
        type Ivars = ();
    }

    unsafe impl NSObjectProtocol for PhotoDelegate {}

    unsafe impl PhotoDelegate {
        // PHPickerViewControllerDelegate (not bound for iOS).
        #[method(picker:didFinishPicking:)]
        fn did_finish(&self, picker: &UIViewController, results: &NSArray<AnyObject>) {
            // The Photos picker doesn't close itself.
            // SAFETY: main thread; no completion block.
            unsafe { picker.dismissViewControllerAnimated_completion(true, None) };
            if results.is_empty() {
                answer(Vec::new());
                return;
            }
            // Loaded on other threads: the last to finish answers.
            let pending = Arc::new(Mutex::new((results.len(), Vec::new())));
            let image = NSString::from_str("public.image");
            for result in results.iter() {
                let pending = Arc::clone(&pending);
                let done = RcBlock::new(move |url: *mut NSURL, _error: *mut NSError| {
                    // The file goes once this returns: copied out now.
                    // SAFETY: UIKit passes a valid URL or null.
                    let copied = unsafe { url.as_ref() }
                        .and_then(|url| unsafe { url.path() })
                        .and_then(|from| {
                            let from = PathBuf::from(from.to_string());
                            let name = from.file_name()?.to_string_lossy().into_owned();
                            let to = unused_path(&cache_dir(), &name);
                            std::fs::copy(&from, &to).ok()?;
                            Some(to.to_string_lossy().into_owned())
                        });
                    let Ok(mut pending) = pending.lock() else {
                        return;
                    };
                    pending.1.extend(copied);
                    pending.0 -= 1;
                    if pending.0 == 0 {
                        answer(std::mem::take(&mut pending.1));
                    }
                });
                // SAFETY: a PHPickerResult's `itemProvider` is an
                // NSItemProvider; the block matches its completion handler.
                unsafe {
                    let provider: Retained<AnyObject> = msg_send_id![result, itemProvider];
                    let _: *mut AnyObject = msg_send![
                        &provider,
                        loadFileRepresentationForTypeIdentifier: &*image,
                        completionHandler: &*done
                    ];
                }
            }
        }
    }
);

impl PhotoDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = mtm.alloc::<Self>().set_ivars(());
        // SAFETY: NSObject's `init`.
        unsafe { msg_send_id![super(this), init] }
    }
}

/// The uniform type for a MIME type (`*/*`: any file; `image/*`: any
/// picture).
fn content_type(mime: &str) -> Option<Retained<UTType>> {
    let id = match mime {
        "*/*" => "public.item",
        "image/*" => "public.image",
        _ => return unsafe { UTType::typeWithMIMEType(&NSString::from_str(mime)) },
    };
    // SAFETY: a plain class method.
    unsafe { UTType::typeWithIdentifier(&NSString::from_str(id)) }
}

/// Show the picker for files of `mimes` (several if `multiple`): Photos for
/// pictures, the Files app's for the rest. The answer comes through
/// [`picker_poll`].
pub fn picker_open(mimes: &[&str], multiple: bool) -> Result<(), String> {
    let mtm = main_thread()?;
    if let Ok(mut picked) = PICKED.lock() {
        *picked = None;
    }
    if mimes == ["image/*"] {
        return photos_open(mtm, multiple);
    }
    let types: Vec<Retained<UTType>> = mimes.iter().filter_map(|m| content_type(m)).collect();
    let types = NSArray::from_vec(types);
    let delegate = DocumentDelegate::new(mtm);
    // SAFETY: main thread; the delegate is kept alive in `DELEGATE`.
    unsafe {
        let picker = UIDocumentPickerViewController::initForOpeningContentTypes_asCopy(
            mtm.alloc(),
            &types,
            true,
        );
        picker.setAllowsMultipleSelection(multiple);
        picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        DELEGATE.with(|d| *d.borrow_mut() = Some(Retained::into_super(delegate)));
        present(mtm, &picker)
    }
}

fn photos_open(mtm: MainThreadMarker, multiple: bool) -> Result<(), String> {
    let delegate = PhotoDelegate::new(mtm);
    // SAFETY: PhotosUI's documented API, on the main thread; the delegate
    // is kept alive in `DELEGATE`.
    unsafe {
        let config: Retained<AnyObject> = msg_send_id![class!(PHPickerConfiguration), new];
        let filter: Retained<AnyObject> = msg_send_id![class!(PHPickerFilter), imagesFilter];
        let _: () = msg_send![&config, setFilter: &*filter];
        // 0: no limit.
        let _: () = msg_send![&config, setSelectionLimit: if multiple { 0isize } else { 1 }];
        // PHPickerConfigurationAssetRepresentationModeCompatible: HEIC
        // photos come as JPEG.
        let _: () = msg_send![&config, setPreferredAssetRepresentationMode: 2isize];
        let alloc: Allocated<UIViewController> =
            msg_send_id![class!(PHPickerViewController), alloc];
        let picker: Retained<UIViewController> =
            msg_send_id![alloc, initWithConfiguration: &*config];
        let _: () = msg_send![&picker, setDelegate: &*delegate];
        DELEGATE.with(|d| *d.borrow_mut() = Some(Retained::into_super(delegate)));
        present(mtm, &picker)
    }
}

/// Saving: iOS's picker exports a file that exists, so the save is written
/// first. Answer at once with where it goes; [`picker_write`] then writes
/// it there and shows the picker.
pub fn picker_create(_mime: &str, name: &str) -> Result<(), String> {
    let path = unused_path(&cache_dir(), name);
    answer(vec![path.to_string_lossy().into_owned()]);
    Ok(())
}

/// The picker's answer, once it has come: the paths of the files picked
/// (none if cancelled).
pub fn picker_poll() -> Option<Vec<String>> {
    PICKED.lock().ok()?.take()
}

/// A picked file's name and contents (the copy then removed).
pub fn picker_read(uri: &str) -> Result<(String, Vec<u8>), String> {
    let path = Path::new(uri);
    let name = path
        .file_name()
        .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let bytes = std::fs::read(path).map_err(|e| format!("Couldn't read {name}: {e}"))?;
    let _ = std::fs::remove_file(path);
    Ok((name, bytes))
}

/// Write a save to `uri` (from [`picker_create`]) and show the picker that
/// puts a copy where the user chooses.
pub fn picker_write(uri: &str, bytes: &[u8]) -> Result<(), String> {
    let mtm = main_thread()?;
    std::fs::write(uri, bytes).map_err(|e| format!("Couldn't write the file: {e}"))?;
    // SAFETY: a plain class method.
    let url = unsafe { NSURL::fileURLWithPath(&NSString::from_str(uri)) };
    let urls = NSArray::from_vec(vec![url]);
    // SAFETY: main thread; no delegate (nothing to do after).
    unsafe {
        let picker =
            UIDocumentPickerViewController::initForExportingURLs_asCopy(mtm.alloc(), &urls, true);
        present(mtm, &picker)
    }
}

/// Move an export at `path` into Documents/Exports, which the Files app
/// shows under On My iPad, Rusty Painter.
pub fn publish_file(path: &Path, mime: &str) -> Result<PublishedFile, String> {
    let dir = crate::app::init::ios_documents().join("Exports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't make the Exports folder: {e}"))?;
    let name = path
        .file_name()
        .map_or_else(|| "export".into(), |n| n.to_string_lossy().into_owned());
    let to = unused_path(&dir, &name);
    if std::fs::rename(path, &to).is_err() {
        std::fs::copy(path, &to).map_err(|e| format!("Couldn't save the export: {e}"))?;
        let _ = std::fs::remove_file(path);
    }
    let shown = to
        .file_name()
        .map_or(name, |n| n.to_string_lossy().into_owned());
    Ok(PublishedFile {
        message: format!("Saved to Files: Rusty Painter/Exports/{shown}"),
        share_uri: Some(to.to_string_lossy().into_owned()),
        share_mime: Some(mime.to_string()),
    })
}

/// Show the share sheet for the file at `uri` (a path from
/// [`publish_file`]): AirDrop, Messages, Save Image to Photos, …
pub fn share_uri(uri: &str, _mime: &str, _title: &str) -> Result<(), String> {
    let mtm = main_thread()?;
    // SAFETY: a plain class method.
    let url = unsafe { NSURL::fileURLWithPath(&NSString::from_str(uri)) };
    // SAFETY: an array of URLs is an array of objects.
    let items: Retained<NSArray> = unsafe { Retained::cast(NSArray::from_vec(vec![url])) };
    let top = top_controller(mtm)?;
    // SAFETY: main thread. On iPad the sheet is a popover, which needs
    // somewhere to point (or UIKit throws): the middle of the screen.
    unsafe {
        let sheet = UIActivityViewController::initWithActivityItems_applicationActivities(
            mtm.alloc(),
            &items,
            None,
        );
        if let (Some(popover), Some(view)) = (sheet.popoverPresentationController(), top.view()) {
            let b = view.bounds();
            popover.setSourceView(Some(&view));
            popover.setSourceRect(CGRect::new(
                CGPoint::new(b.size.width / 2.0, b.size.height / 2.0),
                CGSize::new(0.0, 0.0),
            ));
        }
        top.presentViewController_animated_completion(&sheet, true, None);
    }
    Ok(())
}

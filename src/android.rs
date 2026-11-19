//! Android platform glue: JNI calls for the media store, sharing and
//! file access.

#[cfg(target_os = "android")]
use jni::objects::{JObject, JString, JValue};
#[cfg(target_os = "android")]
use jni::sys::jobject;

/// The app, kept at start-up for its APK assets (see [`extract_pack`]).
#[cfg(target_os = "android")]
pub static APP: std::sync::OnceLock<winit::platform::android::activity::AndroidApp> =
    std::sync::OnceLock::new();

/// Copy the default brush pack out of the APK's assets into `dest`, unless
/// this build's is there already (its manifest, written last, says so).
/// A build without the pack has nothing to copy.
#[cfg(target_os = "android")]
pub fn extract_pack(dest: &std::path::Path) -> Result<(), String> {
    use crate::brush_engine::pack::{Manifest, PACK_ID};
    extract_assets(PACK_ID, dest, |manifest| {
        Ok(vec![
            serde_json::from_slice::<Manifest>(manifest)
                .map_err(|e| e.to_string())?
                .file,
        ])
    })
}

/// Copy the realistic figures (the figure pack) out of the APK's assets
/// into `dest`, as [`extract_pack`] does the brushes.
#[cfg(target_os = "android")]
pub fn extract_figure_pack(dest: &std::path::Path) -> Result<(), String> {
    use crate::canvas::figure::pack::{Manifest, PACK_ID};
    extract_assets(PACK_ID, dest, |manifest| {
        Ok(serde_json::from_slice::<Manifest>(manifest)
            .map_err(|e| e.to_string())?
            .models
            .into_iter()
            .map(|m| m.file)
            .collect())
    })
}

/// Copy the APK's asset folder `dir` into `dest`: its `manifest.json` and
/// the files `files` reads from that manifest, unless they're there already
/// (the same manifest, written last, and files of the same sizes).
#[cfg(target_os = "android")]
fn extract_assets(
    dir: &str,
    dest: &std::path::Path,
    files: impl FnOnce(&[u8]) -> Result<Vec<String>, String>,
) -> Result<(), String> {
    use std::io::Read;
    let Some(app) = APP.get() else {
        return Ok(());
    };
    let assets = app.asset_manager();
    let asset = |name: &str| {
        let path = std::ffi::CString::new(format!("{dir}/{name}")).map_err(|e| e.to_string())?;
        assets
            .open(&path)
            .ok_or_else(|| format!("{name} isn't in the APK"))
    };
    let Ok(mut source) = asset("manifest.json") else {
        return Ok(());
    };
    let mut manifest = Vec::new();
    source
        .read_to_end(&mut manifest)
        .map_err(|e| e.to_string())?;
    let files = files(&manifest)?;
    if files
        .iter()
        .any(|f| std::path::Path::new(f).components().count() != 1)
    {
        return Err("Invalid asset file name".into());
    }
    // (The sizes too: a pack whose manifest didn't change with it.)
    let mut sizes = Vec::with_capacity(files.len());
    for file in &files {
        sizes.push(asset(file)?.length() as u64);
    }
    if std::fs::read(dest.join("manifest.json")).ok().as_deref() == Some(&manifest[..])
        && (files.iter().zip(&sizes))
            .all(|(f, &size)| std::fs::metadata(dest.join(f)).is_ok_and(|m| m.len() == size))
    {
        return Ok(());
    }
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(dest.join("manifest.json"));
    for file in &files {
        let part = dest.join(format!("{file}.part"));
        let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        std::io::copy(&mut asset(file)?, &mut out).map_err(|e| e.to_string())?;
        out.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&part, dest.join(file)).map_err(|e| e.to_string())?;
    }
    crate::project::write_atomically(&dest.join("manifest.json"), &manifest)
}

/// Copy the hand and body tracking models out of the APK's assets into `dest`, as
/// [`extract_pack`] does the brushes.
#[cfg(all(target_os = "android", feature = "pose-capture"))]
pub fn extract_pose_models(dest: &std::path::Path) -> Result<(), String> {
    use crate::pose_track::{Manifest, PACK_ID};
    extract_assets(PACK_ID, dest, |manifest| {
        Ok(serde_json::from_slice::<Manifest>(manifest)
            .map_err(|e| e.to_string())?
            .models
            .into_iter()
            .map(|m| m.file)
            .collect())
    })
}

#[derive(Clone, Debug)]
pub struct PublishedFile {
    pub message: String,
    pub share_uri: Option<String>,
    pub share_mime: Option<String>,
}

/// Run `f` with the JNI environment and the activity. Its local references
/// are freed when it returns: the app's main thread stays attached to the
/// JVM for good, so they'd otherwise pile up, call after call, until the
/// JVM's table of them overflowed and ended the app.
#[cfg(target_os = "android")]
fn with_android_env<T>(
    f: impl FnOnce(&mut jni::JNIEnv<'_>, JObject<'_>) -> Result<T, String>,
) -> Result<T, String> {
    /// References a call may make before the JVM has to grow the frame.
    const LOCAL_REFS: i32 = 64;
    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let activity = unsafe { JObject::from_raw(ctx.context() as jobject) };
    env.with_local_frame(LOCAL_REFS, |env| {
        let result = f(env, activity);
        // A failed call leaves its Java exception pending, which would
        // abort the next JNI call: log it (which clears it).
        if result.is_err() && env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
        Ok::<_, jni::errors::Error>(result)
    })
    .unwrap_or_else(|e| Err(e.to_string()))
}

/// Where `publish_file` puts a file of this type: its kind for
/// `SharedStorage.create` (pictures the gallery shows, videos, everything
/// else in Downloads) and folder.
#[cfg(target_os = "android")]
fn media_collection(mime: &str) -> (i32, &'static str) {
    match mime {
        m if m.starts_with("video/") => (1, "Movies/Rusty Painter"),
        "image/png" | "image/jpeg" | "image/webp" | "image/gif" | "image/tiff" => {
            (0, "Pictures/Rusty Painter")
        }
        _ => (2, "Download/Rusty Painter"),
    }
}

/// Move the file at `path` into the shared storage (Pictures, Movies or
/// Download, under "Rusty Painter"), where other apps see it. It's copied
/// across a bit at a time (a long video is hundreds of MB), into a file
/// other apps don't see until it's whole; if that fails, the copy goes and
/// the file at `path` stays.
#[cfg(target_os = "android")]
pub fn publish_file(path: &std::path::Path, mime: &str) -> Result<PublishedFile, String> {
    let mut source =
        std::fs::File::open(path).map_err(|e| format!("Couldn't read the export: {e}"))?;
    let file_name = path
        .file_name()
        .map_or_else(|| "export".into(), |n| n.to_string_lossy().into_owned());
    let (kind, dir) = media_collection(mime);
    let uri = with_android_env(|env, activity| {
        let class = helper_class(env, &activity, "SharedStorage")?;
        let args = [
            env.new_string(&file_name),
            env.new_string(mime),
            env.new_string(dir),
        ];
        let [name, mime, dir] = args.map(|s| s.map(JObject::from));
        let uri = env
            .call_static_method(
                class,
                "create",
                "(Landroid/content/Context;ILjava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
                &[
                    JValue::Object(&activity),
                    JValue::Int(kind),
                    JValue::Object(&name.map_err(jerr)?),
                    JValue::Object(&mime.map_err(jerr)?),
                    JValue::Object(&dir.map_err(jerr)?),
                ],
            )
            .and_then(|v| v.l())
            .map_err(|_| "Android refused to create the file".to_string())?;
        env.get_string(&JString::from(uri))
            .map(String::from)
            .map_err(jerr)
    })?;
    let storage = |method: &str| {
        with_android_env(|env, activity| {
            let class = helper_class(env, &activity, "SharedStorage")?;
            let juri = env.new_string(&uri).map_err(jerr)?;
            env.call_static_method(
                class,
                method,
                "(Landroid/content/Context;Ljava/lang/String;)V",
                &[JValue::Object(&activity), JValue::Object(&juri)],
            )
            .map_err(jerr)?;
            Ok(())
        })
    };
    let written = open_fd(&uri, "w").and_then(|mut out| {
        std::io::copy(&mut source, &mut out)
            .map_err(|e| format!("Couldn't save the export: {e}"))?;
        drop(out);
        storage("finish")
    });
    match written {
        Ok(()) => {
            let _ = std::fs::remove_file(path);
            Ok(PublishedFile {
                message: format!("Saved to {dir}/{file_name}"),
                share_uri: Some(uri),
                share_mime: Some(mime.to_string()),
            })
        }
        Err(err) => {
            let _ = storage("discard");
            Err(err)
        }
    }
}

/// A file descriptor for content `uri`, opened by Android (`mode` "r",
/// "w" or "wt"), as a file to read or write here, on any thread.
#[cfg(target_os = "android")]
fn open_fd(uri: &str, mode: &str) -> Result<std::fs::File, String> {
    use std::os::fd::FromRawFd;
    let fd = with_android_env(|env, activity| {
        let class = picker_class(env, &activity)?;
        let juri = env.new_string(uri).map_err(jerr)?;
        let jmode = env.new_string(mode).map_err(jerr)?;
        env.call_static_method(
            class,
            "openFd",
            "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)I",
            &[
                JValue::Object(&activity),
                JValue::Object(&juri),
                JValue::Object(&jmode),
            ],
        )
        .and_then(|v| v.i())
        .map_err(|_| "Couldn't open the file".to_string())
    })?;
    if fd < 0 {
        return Err("Couldn't open the file".to_string());
    }
    // SAFETY: `detachFd` handed the descriptor over: it's ours alone to
    // use and close.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

/// The app's cache folder, for files on their way to shared storage.
#[cfg(target_os = "android")]
pub fn cache_dir() -> std::path::PathBuf {
    let dir = crate::ANDROID_DATA
        .get()
        .and_then(|d| d.parent())
        .map_or_else(std::env::temp_dir, |d| d.join("cache"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The Java helpers (`android/java`: the file picker, the keyboard), built
/// into `classes.dex` by `scripts/build-android-dex.sh`.
#[cfg(target_os = "android")]
static HELPER_DEX: &[u8] = include_bytes!("../android/classes.dex");
#[cfg(target_os = "android")]
static HELPER_LOADER: std::sync::OnceLock<jni::objects::GlobalRef> = std::sync::OnceLock::new();

/// The helper classes loaded so far, by name (a handful, kept for good).
#[cfg(target_os = "android")]
static HELPER_CLASSES: std::sync::Mutex<Vec<(&'static str, &'static jni::objects::GlobalRef)>> =
    std::sync::Mutex::new(Vec::new());

/// Helper class `name` (like "FilePicker"), loaded from the embedded dex
/// the first time and kept (a class lookup on every call cost a few
/// local references and a trip through the class loader).
#[cfg(target_os = "android")]
fn helper_class(
    env: &mut jni::JNIEnv<'_>,
    activity: &JObject<'_>,
    name: &'static str,
) -> Result<&'static jni::objects::GlobalRef, String> {
    let known = |classes: &[(&str, &'static jni::objects::GlobalRef)]| {
        classes.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
    };
    if let Some(class) = HELPER_CLASSES.lock().ok().and_then(|c| known(&c)) {
        return Ok(class);
    }
    let class = load_helper_class(env, activity, name)?;
    let global = env.new_global_ref(class).map_err(jerr)?;
    let mut classes = HELPER_CLASSES.lock().map_err(|e| e.to_string())?;
    // (Another thread may have loaded it meanwhile.)
    if let Some(class) = known(&classes) {
        return Ok(class);
    }
    let global: &'static jni::objects::GlobalRef = Box::leak(Box::new(global));
    classes.push((name, global));
    Ok(global)
}

/// Load helper class `name` from the embedded dex.
#[cfg(target_os = "android")]
fn load_helper_class<'a>(
    env: &mut jni::JNIEnv<'a>,
    activity: &JObject<'_>,
    name: &str,
) -> Result<jni::objects::JClass<'a>, String> {
    if HELPER_LOADER.get().is_none() {
        let dex = env.byte_array_from_slice(HELPER_DEX).map_err(jerr)?;
        let buffer = env
            .call_static_method(
                "java/nio/ByteBuffer",
                "wrap",
                "([B)Ljava/nio/ByteBuffer;",
                &[JValue::Object(&dex)],
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        let parent = env
            .call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])
            .and_then(|v| v.l())
            .map_err(jerr)?;
        let loader = env
            .new_object(
                "dalvik/system/InMemoryDexClassLoader",
                "(Ljava/nio/ByteBuffer;Ljava/lang/ClassLoader;)V",
                &[JValue::Object(&buffer), JValue::Object(&parent)],
            )
            .map_err(jerr)?;
        let _ = HELPER_LOADER.set(env.new_global_ref(loader).map_err(jerr)?);
    }
    let loader = HELPER_LOADER.get().expect("set above");
    let name = env
        .new_string(format!("io.vectra.rustypainter.{name}"))
        .map_err(jerr)?;
    let class = env
        .call_method(
            loader.as_obj(),
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&name)],
        )
        .and_then(|v| v.l())
        .map_err(jerr)?;
    Ok(jni::objects::JClass::from(class))
}

#[cfg(target_os = "android")]
fn picker_class(
    env: &mut jni::JNIEnv<'_>,
    activity: &JObject<'_>,
) -> Result<&'static jni::objects::GlobalRef, String> {
    helper_class(env, activity, "FilePicker")
}

/// Back no longer ends the app but reaches egui as Escape (see
/// `BackGuard.java`), and low memory is heard of (`MemoryWatch.java`).
#[cfg(target_os = "android")]
pub fn guard_back() -> Result<(), String> {
    with_android_env(|env, activity| {
        for helper in ["BackGuard", "MemoryWatch"] {
            let class = helper_class(env, &activity, helper)?;
            env.call_static_method(
                class,
                "install",
                "(Landroid/app/Activity;)V",
                &[JValue::Object(&activity)],
            )
            .map_err(jerr)?;
        }
        Ok(())
    })
}

/// Android said memory is running low (see `MemoryWatch.java`).
#[cfg(target_os = "android")]
static LOW_MEMORY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether memory ran low since last asked (polled once a frame).
#[cfg(target_os = "android")]
pub fn take_low_memory() -> bool {
    LOW_MEMORY.swap(false, std::sync::atomic::Ordering::Relaxed)
}

/// The text on the clipboard (what the camera copied from a QR code, say).
#[cfg(target_os = "android")]
pub fn clipboard_text() -> Result<String, String> {
    with_android_env(|env, activity| {
        let class = helper_class(env, &activity, "Clipboard")?;
        let text = env
            .call_static_method(
                class,
                "text",
                "(Landroid/app/Activity;)Ljava/lang/String;",
                &[JValue::Object(&activity)],
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        if text.is_null() {
            return Err("The clipboard has no text".into());
        }
        env.get_string(&JString::from(text))
            .map(String::from)
            .map_err(jerr)
    })
}

/// Show or hide the soft keyboard (NativeActivity can't by itself).
#[cfg(target_os = "android")]
pub fn show_keyboard(on: bool) -> Result<(), String> {
    with_android_env(|env, activity| {
        let class = helper_class(env, &activity, "TextInput")?;
        env.call_static_method(
            class,
            "show",
            "(Landroid/app/Activity;Z)V",
            &[JValue::Object(&activity), JValue::Bool(on.into())],
        )
        .map_err(jerr)?;
        Ok(())
    })
}

#[cfg(target_os = "android")]
static KEYBOARD_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Once a frame: the keyboard shown while a text field has focus, and what
/// was typed on it. Back (as Escape) comes whether or not the keyboard is
/// up; typing that arrives with no text field wanting it is dropped.
#[cfg(target_os = "android")]
pub fn keyboard_input(wanted: bool) -> Vec<eframe::egui::Event> {
    use std::sync::atomic::Ordering;
    if KEYBOARD_SHOWN.swap(wanted, Ordering::Relaxed) != wanted
        && let Err(err) = show_keyboard(wanted)
    {
        log::error!("Keyboard: {err}");
    }
    let mut events = typed();
    if !wanted {
        events.retain(|e| {
            matches!(
                e,
                eframe::egui::Event::Key {
                    key: eframe::egui::Key::Escape,
                    ..
                }
            )
        });
    }
    events
}

/// What was typed on the soft keyboard since last asked, Back pressed (as
/// Escape), and whether memory ran low (see [`take_low_memory`]).
#[cfg(target_os = "android")]
fn typed() -> Vec<eframe::egui::Event> {
    use eframe::egui::{Event, Key, Modifiers};
    let key = |key, pressed| Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: Modifiers::NONE,
    };
    with_android_env(|env, activity| {
        let class = helper_class(env, &activity, "TextInput")?;
        let mut events = Vec::new();
        loop {
            let s = env
                .call_static_method(class, "poll", "()Ljava/lang/String;", &[])
                .and_then(|v| v.l())
                .map_err(jerr)?;
            if s.is_null() {
                return Ok(events);
            }
            let s = JString::from(s);
            let text: String = env.get_string(&s).map_err(jerr)?.into();
            let _ = env.delete_local_ref(s);
            let k = match text.split_at_checked(1) {
                Some(("t", typed)) => {
                    events.push(Event::Text(typed.to_string()));
                    continue;
                }
                Some(("m", _)) => {
                    LOW_MEMORY.store(true, std::sync::atomic::Ordering::Relaxed);
                    continue;
                }
                Some(("b", _)) => Key::Backspace,
                Some(("d", _)) => Key::Delete,
                Some(("x", _)) => Key::Escape,
                Some(("e", _)) => Key::Enter,
                _ => continue,
            };
            events.extend([key(k, true), key(k, false)]);
        }
    })
    .unwrap_or_default()
}

/// Open the system picker for existing files (`mimes` like "image/*";
/// "*/*" for any). The answer comes from [`picker_poll`].
#[cfg(target_os = "android")]
pub fn picker_open(mimes: &[&str], multiple: bool) -> Result<(), String> {
    with_android_env(|env, activity| {
        let class = picker_class(env, &activity)?;
        let array = env
            .new_object_array(mimes.len() as i32, "java/lang/String", JObject::null())
            .map_err(jerr)?;
        for (i, m) in mimes.iter().enumerate() {
            let s = env.new_string(m).map_err(jerr)?;
            env.set_object_array_element(&array, i as i32, s)
                .map_err(jerr)?;
        }
        env.call_static_method(
            class,
            "open",
            "(Landroid/app/Activity;[Ljava/lang/String;Z)V",
            &[
                JValue::Object(&activity),
                JValue::Object(&array),
                JValue::Bool(multiple.into()),
            ],
        )
        .map_err(jerr)?;
        Ok(())
    })
}

/// Open the system picker to create a file called `name`. The answer comes
/// from [`picker_poll`].
#[cfg(target_os = "android")]
pub fn picker_create(mime: &str, name: &str) -> Result<(), String> {
    with_android_env(|env, activity| {
        let class = picker_class(env, &activity)?;
        let mime = env.new_string(mime).map_err(jerr)?;
        let name = env.new_string(name).map_err(jerr)?;
        env.call_static_method(
            class,
            "create",
            "(Landroid/app/Activity;Ljava/lang/String;Ljava/lang/String;)V",
            &[
                JValue::Object(&activity),
                JValue::Object(&mime),
                JValue::Object(&name),
            ],
        )
        .map_err(jerr)?;
        Ok(())
    })
}

/// The picker's answer once it closes: the picked URIs (none when
/// cancelled). `None` while it's open.
#[cfg(target_os = "android")]
pub fn picker_poll() -> Option<Vec<String>> {
    with_android_env(|env, activity| {
        let class = picker_class(env, &activity)?;
        let array = env
            .call_static_method(class, "poll", "()[Ljava/lang/String;", &[])
            .and_then(|v| v.l())
            .map_err(jerr)?;
        if array.is_null() {
            return Ok(None);
        }
        let array = jni::objects::JObjectArray::from(array);
        let n = env.get_array_length(&array).map_err(jerr)?;
        let mut uris = Vec::new();
        for i in 0..n {
            let s = JString::from(env.get_object_array_element(&array, i).map_err(jerr)?);
            uris.push(env.get_string(&s).map_err(jerr)?.into());
        }
        Ok(Some(uris))
    })
    .unwrap_or_else(|e| {
        log::error!("File picker: {e}");
        Some(Vec::new())
    })
}

/// A picked file's name, and the file opened to read (on a job's thread:
/// only opening it happens here).
#[cfg(target_os = "android")]
pub fn picker_read(uri: &str) -> Result<(String, crate::app::import::FileSource), String> {
    let name = with_android_env(|env, activity| {
        let class = picker_class(env, &activity)?;
        let juri = env.new_string(uri).map_err(jerr)?;
        let name = env
            .call_static_method(
                class,
                "displayName",
                "(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;",
                &[JValue::Object(&activity), JValue::Object(&juri)],
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        env.get_string(&JString::from(name))
            .map(String::from)
            .map_err(jerr)
    })?;
    let file = open_fd(uri, "r").map_err(|_| format!("Couldn't read {name}"))?;
    Ok((
        name,
        crate::app::import::FileSource::File(std::sync::Arc::new(file)),
    ))
}

/// A file the picker created, opened to write over (on a job's thread).
#[cfg(target_os = "android")]
pub fn picker_open_file(uri: &str) -> Result<std::fs::File, String> {
    // "wt": truncated, should the picked file already have content.
    open_fd(uri, "wt")
}

#[cfg(target_os = "android")]
pub fn share_uri(uri: &str, mime: &str, title: &str) -> Result<(), String> {
    with_android_env(|env, activity| {
        let action = env
            .get_static_field(
                "android/content/Intent",
                "ACTION_SEND",
                "Ljava/lang/String;",
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;

        let intent = env
            .new_object(
                "android/content/Intent",
                "(Ljava/lang/String;)V",
                &[JValue::Object(&action)],
            )
            .map_err(|e| e.to_string())?;

        let mime = env.new_string(mime).map_err(|e| e.to_string())?;
        env.call_method(
            &intent,
            "setType",
            "(Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&JObject::from(mime))],
        )
        .map_err(|e| e.to_string())?;

        let uri = env.new_string(uri).map_err(|e| e.to_string())?;
        let parsed_uri = env
            .call_static_method(
                "android/net/Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValue::Object(&JObject::from(uri))],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;

        let extra_stream = env
            .get_static_field(
                "android/content/Intent",
                "EXTRA_STREAM",
                "Ljava/lang/String;",
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;

        env.call_method(
            &intent,
            "putExtra",
            "(Ljava/lang/String;Landroid/os/Parcelable;)Landroid/content/Intent;",
            &[JValue::Object(&extra_stream), JValue::Object(&parsed_uri)],
        )
        .map_err(|e| e.to_string())?;

        let grant_read_flag = env
            .get_static_field(
                "android/content/Intent",
                "FLAG_GRANT_READ_URI_PERMISSION",
                "I",
            )
            .map_err(|e| e.to_string())?
            .i()
            .map_err(|e| e.to_string())?;

        env.call_method(
            &intent,
            "addFlags",
            "(I)Landroid/content/Intent;",
            &[JValue::Int(grant_read_flag)],
        )
        .map_err(|e| e.to_string())?;

        let title = env.new_string(title).map_err(|e| e.to_string())?;
        let chooser = env
            .call_static_method(
                "android/content/Intent",
                "createChooser",
                "(Landroid/content/Intent;Ljava/lang/CharSequence;)Landroid/content/Intent;",
                &[
                    JValue::Object(&intent),
                    JValue::Object(&JObject::from(title)),
                ],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;

        env.call_method(
            &activity,
            "startActivity",
            "(Landroid/content/Intent;)V",
            &[JValue::Object(&chooser)],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })
}

/// An image in the device's photo library.
#[derive(Clone, Debug)]
pub struct GalleryImage {
    pub id: i64,
    pub name: String,
}

#[cfg(target_os = "android")]
fn jerr(e: jni::errors::Error) -> String {
    e.to_string()
}

#[cfg(target_os = "android")]
fn image_permissions(env: &mut jni::JNIEnv<'_>) -> Result<Vec<&'static str>, String> {
    let sdk = env
        .get_static_field("android/os/Build$VERSION", "SDK_INT", "I")
        .and_then(|v| v.i())
        .map_err(jerr)?;
    Ok(match sdk {
        // Android 14 can grant access to just the photos the user picks.
        34.. => vec![
            "android.permission.READ_MEDIA_IMAGES",
            "android.permission.READ_MEDIA_VISUAL_USER_SELECTED",
        ],
        33 => vec!["android.permission.READ_MEDIA_IMAGES"],
        _ => vec!["android.permission.READ_EXTERNAL_STORAGE"],
    })
}

#[cfg(target_os = "android")]
fn content_resolver<'a>(
    env: &mut jni::JNIEnv<'a>,
    activity: &JObject<'_>,
) -> Result<JObject<'a>, String> {
    env.call_method(
        activity,
        "getContentResolver",
        "()Landroid/content/ContentResolver;",
        &[],
    )
    .and_then(|v| v.l())
    .map_err(jerr)
}

#[cfg(target_os = "android")]
fn images_collection<'a>(env: &mut jni::JNIEnv<'a>) -> Result<JObject<'a>, String> {
    env.get_static_field(
        "android/provider/MediaStore$Images$Media",
        "EXTERNAL_CONTENT_URI",
        "Landroid/net/Uri;",
    )
    .and_then(|v| v.l())
    .map_err(jerr)
}

#[cfg(target_os = "android")]
fn image_uri<'a>(env: &mut jni::JNIEnv<'a>, id: i64) -> Result<JObject<'a>, String> {
    let collection = images_collection(env)?;
    env.call_static_method(
        "android/content/ContentUris",
        "withAppendedId",
        "(Landroid/net/Uri;J)Landroid/net/Uri;",
        &[JValue::Object(&collection), JValue::Long(id)],
    )
    .and_then(|v| v.l())
    .map_err(jerr)
}

#[cfg(target_os = "android")]
fn string_array<'a>(
    env: &mut jni::JNIEnv<'a>,
    items: &[&str],
) -> Result<jni::objects::JObjectArray<'a>, String> {
    let array = env
        .new_object_array(items.len() as i32, "java/lang/String", JObject::null())
        .map_err(jerr)?;
    for (i, item) in items.iter().enumerate() {
        let s = env.new_string(item).map_err(jerr)?;
        env.set_object_array_element(&array, i as i32, &s)
            .map_err(jerr)?;
    }
    Ok(array)
}

/// Whether the app may read the photo library (fully or the photos the
/// user picked).
#[cfg(target_os = "android")]
pub fn has_image_access() -> bool {
    with_android_env(|env, activity| {
        for perm in image_permissions(env)? {
            let name = env.new_string(perm).map_err(jerr)?;
            let granted = env
                .call_method(
                    &activity,
                    "checkSelfPermission",
                    "(Ljava/lang/String;)I",
                    &[JValue::Object(&JObject::from(name))],
                )
                .and_then(|v| v.i())
                .map_err(jerr)?;
            if granted == 0 {
                return Ok(true);
            }
        }
        Ok(false)
    })
    .unwrap_or(false)
}

/// Show the system prompt for photo access. The answer isn't reported
/// back; poll [`has_image_access`].
#[cfg(target_os = "android")]
pub fn request_image_access() -> Result<(), String> {
    with_android_env(|env, activity| {
        let perms = image_permissions(env)?;
        let array = string_array(env, &perms)?;
        env.call_method(
            &activity,
            "requestPermissions",
            "([Ljava/lang/String;I)V",
            &[JValue::Object(&array), JValue::Int(7)],
        )
        .map_err(jerr)?;
        Ok(())
    })
}

/// The newest `limit` images in the photo library.
#[cfg(target_os = "android")]
pub fn list_images(limit: usize) -> Result<Vec<GalleryImage>, String> {
    with_android_env(|env, activity| {
        let resolver = content_resolver(env, &activity)?;
        let collection = images_collection(env)?;
        let projection = string_array(env, &["_id", "_display_name"])?;
        let sort = env.new_string("date_modified DESC").map_err(jerr)?;
        let cursor = env
            .call_method(
                &resolver,
                "query",
                "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
                &[
                    JValue::Object(&collection),
                    JValue::Object(&projection),
                    JValue::Object(&JObject::null()),
                    JValue::Object(&JObject::null()),
                    JValue::Object(&JObject::from(sort)),
                ],
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        if cursor.is_null() {
            return Err("The photo library isn't available".to_string());
        }
        let mut out = Vec::new();
        while out.len() < limit
            && env
                .call_method(&cursor, "moveToNext", "()Z", &[])
                .and_then(|v| v.z())
                .map_err(jerr)?
        {
            let id = env
                .call_method(&cursor, "getLong", "(I)J", &[JValue::Int(0)])
                .and_then(|v| v.j())
                .map_err(jerr)?;
            let name_obj = env
                .call_method(
                    &cursor,
                    "getString",
                    "(I)Ljava/lang/String;",
                    &[JValue::Int(1)],
                )
                .and_then(|v| v.l())
                .map_err(jerr)?;
            let name = if name_obj.is_null() {
                String::from("Image")
            } else {
                let js = JString::from(name_obj);
                let name: String = env.get_string(&js).map(Into::into).unwrap_or_default();
                // Keep the local reference table small over long lists.
                let _ = env.delete_local_ref(js);
                name
            };
            out.push(GalleryImage { id, name });
        }
        let _ = env.call_method(&cursor, "close", "()V", &[]);
        Ok(out)
    })
}

/// A small preview of image `id`: `(width, height, RGBA bytes)`.
#[cfg(target_os = "android")]
pub fn load_thumbnail(id: i64, size: i32) -> Result<(usize, usize, Vec<u8>), String> {
    with_android_env(|env, activity| {
        let resolver = content_resolver(env, &activity)?;
        let uri = image_uri(env, id)?;
        let dims = env
            .new_object(
                "android/util/Size",
                "(II)V",
                &[JValue::Int(size), JValue::Int(size)],
            )
            .map_err(jerr)?;
        let bitmap = env
            .call_method(
                &resolver,
                "loadThumbnail",
                "(Landroid/net/Uri;Landroid/util/Size;Landroid/os/CancellationSignal;)Landroid/graphics/Bitmap;",
                &[JValue::Object(&uri), JValue::Object(&dims), JValue::Object(&JObject::null())],
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        let w = env
            .call_method(&bitmap, "getWidth", "()I", &[])
            .and_then(|v| v.i())
            .map_err(jerr)?;
        let h = env
            .call_method(&bitmap, "getHeight", "()I", &[])
            .and_then(|v| v.i())
            .map_err(jerr)?;
        let pixels = env.new_int_array(w * h).map_err(jerr)?;
        env.call_method(
            &bitmap,
            "getPixels",
            "([IIIIIII)V",
            &[
                JValue::Object(&pixels),
                JValue::Int(0),
                JValue::Int(w),
                JValue::Int(0),
                JValue::Int(0),
                JValue::Int(w),
                JValue::Int(h),
            ],
        )
        .map_err(jerr)?;
        let mut argb = vec![0i32; (w * h) as usize];
        env.get_int_array_region(&pixels, 0, &mut argb)
            .map_err(jerr)?;
        let _ = env.call_method(&bitmap, "recycle", "()V", &[]);
        let rgba = argb
            .iter()
            .flat_map(|&p| {
                let p = p as u32;
                [(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]
            })
            .collect();
        Ok((w as usize, h as usize, rgba))
    })
}

/// The encoded file bytes of image `id`.
#[cfg(target_os = "android")]
pub fn read_image(id: i64) -> Result<Vec<u8>, String> {
    with_android_env(|env, activity| {
        let resolver = content_resolver(env, &activity)?;
        let uri = image_uri(env, id)?;
        let stream = env
            .call_method(
                &resolver,
                "openInputStream",
                "(Landroid/net/Uri;)Ljava/io/InputStream;",
                &[JValue::Object(&uri)],
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        if stream.is_null() {
            return Err("Couldn't open the image".to_string());
        }
        const CHUNK: usize = 1 << 16;
        let buffer = env.new_byte_array(CHUNK as i32).map_err(jerr)?;
        let mut chunk = vec![0i8; CHUNK];
        let mut out = Vec::new();
        loop {
            let n = env
                .call_method(&stream, "read", "([B)I", &[JValue::Object(&buffer)])
                .and_then(|v| v.i())
                .map_err(jerr)?;
            if n < 0 {
                break;
            }
            let n = n as usize;
            env.get_byte_array_region(&buffer, 0, &mut chunk[..n])
                .map_err(jerr)?;
            out.extend(chunk[..n].iter().map(|&b| b as u8));
        }
        let _ = env.call_method(&stream, "close", "()V", &[]);
        Ok(out)
    })
}

/// A picture the image decoders can't read but Android can (HEIC/HEIF,
/// what most phone cameras save, or AVIF): decoded by `BitmapFactory`, in
/// sRGB, not premultiplied.
// Decoded at full size: a 48 MP HEIC is ~190 MB here; `inSampleSize` would
// shrink it while decoding if that's ever too much.
#[cfg(target_os = "android")]
pub fn decode_image(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    let len = i32::try_from(bytes.len()).map_err(|_| "The image is too big".to_string())?;
    with_android_env(|env, _activity| {
        let options = env
            .new_object("android/graphics/BitmapFactory$Options", "()V", &[])
            .map_err(jerr)?;
        env.set_field(&options, "inPremultiplied", "Z", JValue::Bool(0))
            .map_err(jerr)?;
        let config = env
            .get_static_field(
                "android/graphics/Bitmap$Config",
                "ARGB_8888",
                "Landroid/graphics/Bitmap$Config;",
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        env.set_field(
            &options,
            "inPreferredConfig",
            "Landroid/graphics/Bitmap$Config;",
            JValue::Object(&config),
        )
        .map_err(jerr)?;
        // Converted to sRGB (a phone's HEIC is often Display P3), as the
        // image decoders' pictures are before going into the document.
        let named = env
            .get_static_field(
                "android/graphics/ColorSpace$Named",
                "SRGB",
                "Landroid/graphics/ColorSpace$Named;",
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        let srgb = env
            .call_static_method(
                "android/graphics/ColorSpace",
                "get",
                "(Landroid/graphics/ColorSpace$Named;)Landroid/graphics/ColorSpace;",
                &[JValue::Object(&named)],
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        env.set_field(
            &options,
            "inPreferredColorSpace",
            "Landroid/graphics/ColorSpace;",
            JValue::Object(&srgb),
        )
        .map_err(jerr)?;
        let array = env.byte_array_from_slice(bytes).map_err(jerr)?;
        let bitmap = env
            .call_static_method(
                "android/graphics/BitmapFactory",
                "decodeByteArray",
                "([BIILandroid/graphics/BitmapFactory$Options;)Landroid/graphics/Bitmap;",
                &[
                    JValue::Object(&array),
                    JValue::Int(0),
                    JValue::Int(len),
                    JValue::Object(&options),
                ],
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        if bitmap.is_null() {
            return Err("This kind of image can't be opened".to_string());
        }
        let int = |env: &mut jni::JNIEnv<'_>, name: &str| {
            env.call_method(&bitmap, name, "()I", &[])
                .and_then(|v| v.i())
                .map_err(jerr)
        };
        let (w, h, row) = (
            int(env, "getWidth")?,
            int(env, "getHeight")?,
            int(env, "getRowBytes")?,
        );
        let result = (|| {
            // ARGB_8888 rows, tightly packed: R, G, B, A bytes.
            if w <= 0 || h <= 0 || row != w * 4 {
                return Err("The image decoded to an unexpected layout".to_string());
            }
            let mut pixels = vec![0u8; w as usize * h as usize * 4];
            // Android writes straight into `pixels`.
            let buffer = unsafe { env.new_direct_byte_buffer(pixels.as_mut_ptr(), pixels.len()) }
                .map_err(jerr)?;
            env.call_method(
                &bitmap,
                "copyPixelsToBuffer",
                "(Ljava/nio/Buffer;)V",
                &[JValue::Object(&buffer)],
            )
            .map_err(jerr)?;
            image::RgbaImage::from_raw(w as u32, h as u32, pixels)
                .ok_or_else(|| "The image decoded to an unexpected size".to_string())
        })();
        // Its pixels are native memory: freed now, not whenever Java's
        // garbage collector runs.
        let _ = env.call_method(&bitmap, "recycle", "()V", &[]);
        result
    })
}

/// The cameras (see `CameraFeed.java`), front ones first: each one's id
/// and a name to show.
#[cfg(all(target_os = "android", feature = "pose-capture"))]
pub fn camera_list() -> Result<Vec<(String, String)>, String> {
    with_android_env(|env, activity| {
        let class = helper_class(env, &activity, "CameraFeed")?;
        let array = env
            .call_static_method(
                class,
                "list",
                "(Landroid/app/Activity;)[Ljava/lang/String;",
                &[JValue::Object(&activity)],
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        let array = jni::objects::JObjectArray::from(array);
        let n = env.get_array_length(&array).map_err(jerr)?;
        let mut out = Vec::new();
        for i in 0..n {
            let s = JString::from(env.get_object_array_element(&array, i).map_err(jerr)?);
            let entry: String = env.get_string(&s).map_err(jerr)?.into();
            if let Some((id, name)) = entry.split_once('\n') {
                out.push((id.to_string(), name.to_string()));
            }
        }
        Ok(out)
    })
}

/// Camera `id` started at about `width` × `height`: its frame buffer (kept
/// alive by the reference), mapped, and its size in bytes.
#[cfg(all(target_os = "android", feature = "pose-capture"))]
pub fn camera_start(
    id: &str,
    width: i32,
    height: i32,
) -> Result<(jni::objects::GlobalRef, *mut u8, usize), String> {
    with_android_env(|env, activity| {
        let class = helper_class(env, &activity, "CameraFeed")?;
        let id = env.new_string(id).map_err(jerr)?;
        let buffer = env
            .call_static_method(
                class,
                "start",
                "(Landroid/app/Activity;Ljava/lang/String;II)Ljava/nio/ByteBuffer;",
                &[
                    JValue::Object(&activity),
                    JValue::Object(&JObject::from(id)),
                    JValue::Int(width),
                    JValue::Int(height),
                ],
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        let buffer = jni::objects::JByteBuffer::from(buffer);
        let address = env.get_direct_buffer_address(&buffer).map_err(jerr)?;
        let capacity = env.get_direct_buffer_capacity(&buffer).map_err(jerr)?;
        let global = env.new_global_ref(&buffer).map_err(jerr)?;
        Ok((global, address, capacity))
    })
}

/// The newest frame's number if newer than `last` (the buffer is then the
/// Rust side's until [`camera_end`]), else -1.
#[cfg(all(target_os = "android", feature = "pose-capture"))]
pub fn camera_begin(last: i64) -> Result<i64, String> {
    with_android_env(|env, activity| {
        let class = helper_class(env, &activity, "CameraFeed")?;
        env.call_static_method(class, "begin", "(J)J", &[JValue::Long(last)])
            .and_then(|v| v.j())
            .map_err(jerr)
    })
}

/// Done reading the frame buffer.
#[cfg(all(target_os = "android", feature = "pose-capture"))]
pub fn camera_end() -> Result<(), String> {
    with_android_env(|env, activity| {
        let class = helper_class(env, &activity, "CameraFeed")?;
        env.call_static_method(class, "end", "()V", &[])
            .map_err(jerr)?;
        Ok(())
    })
}

/// Why the camera stopped by itself, if it did.
#[cfg(all(target_os = "android", feature = "pose-capture"))]
pub fn camera_error() -> Option<String> {
    with_android_env(|env, activity| {
        let class = helper_class(env, &activity, "CameraFeed")?;
        let error = env
            .call_static_method(class, "error", "()Ljava/lang/String;", &[])
            .and_then(|v| v.l())
            .map_err(jerr)?;
        if error.is_null() {
            return Ok(None);
        }
        env.get_string(&JString::from(error))
            .map(|s| Some(String::from(s)))
            .map_err(jerr)
    })
    .ok()
    .flatten()
}

#[cfg(all(target_os = "android", feature = "pose-capture"))]
pub fn camera_stop() {
    let stopped = with_android_env(|env, activity| {
        let class = helper_class(env, &activity, "CameraFeed")?;
        env.call_static_method(class, "stop", "()V", &[])
            .map_err(jerr)?;
        Ok(())
    });
    if let Err(err) = stopped {
        log::warn!("Couldn't stop the camera: {err}");
    }
}

/// Whether the app may use the camera.
#[cfg(all(target_os = "android", feature = "pose-capture"))]
pub fn has_camera_access() -> bool {
    with_android_env(|env, activity| {
        let name = env.new_string("android.permission.CAMERA").map_err(jerr)?;
        let granted = env
            .call_method(
                &activity,
                "checkSelfPermission",
                "(Ljava/lang/String;)I",
                &[JValue::Object(&JObject::from(name))],
            )
            .and_then(|v| v.i())
            .map_err(jerr)?;
        Ok(granted == 0)
    })
    .unwrap_or(false)
}

/// Show the system prompt for the camera. The answer isn't reported back;
/// poll [`has_camera_access`].
#[cfg(all(target_os = "android", feature = "pose-capture"))]
pub fn request_camera_access() -> Result<(), String> {
    with_android_env(|env, activity| {
        let array = string_array(env, &["android.permission.CAMERA"])?;
        env.call_method(
            &activity,
            "requestPermissions",
            "([Ljava/lang/String;I)V",
            &[JValue::Object(&array), JValue::Int(8)],
        )
        .map_err(jerr)?;
        Ok(())
    })
}

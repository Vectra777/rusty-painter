//! Android platform glue: JNI calls for the media store, sharing and
//! file access.

#[cfg(target_os = "android")]
use jni::objects::{JObject, JString, JValue};
#[cfg(target_os = "android")]
use jni::sys::jobject;

#[derive(Clone, Debug)]
pub struct AndroidExport {
    pub message: String,
    pub share_uri: Option<String>,
    pub share_mime: Option<String>,
}

#[cfg(target_os = "android")]
fn with_android_env<T>(
    f: impl FnOnce(&mut jni::JNIEnv<'_>, JObject<'_>) -> Result<T, String>,
) -> Result<T, String> {
    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    let activity = unsafe { JObject::from_raw(ctx.context() as jobject) };
    let result = f(&mut env, activity);
    // A failed call leaves its Java exception pending, which would abort the
    // next JNI call: log it (which clears it).
    if result.is_err() && env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    result
}

#[cfg(target_os = "android")]
fn put_string(
    env: &mut jni::JNIEnv<'_>,
    values: &JObject<'_>,
    key: &str,
    value: &str,
) -> Result<(), String> {
    let key = env.new_string(key).map_err(|e| e.to_string())?;
    let value = env.new_string(value).map_err(|e| e.to_string())?;
    env.call_method(
        values,
        "put",
        "(Ljava/lang/String;Ljava/lang/String;)V",
        &[
            JValue::Object(&JObject::from(key)),
            JValue::Object(&JObject::from(value)),
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(target_os = "android")]
fn uri_to_string(env: &mut jni::JNIEnv<'_>, uri: &JObject<'_>) -> Result<String, String> {
    let uri_string = env
        .call_method(uri, "toString", "()Ljava/lang/String;", &[])
        .map_err(|e| e.to_string())?
        .l()
        .map_err(|e| e.to_string())?;
    let uri_string = JString::from(uri_string);
    env.get_string(&uri_string)
        .map(|s| s.into())
        .map_err(|e| e.to_string())
}

/// Where `publish_file` puts a file of this type, by MediaStore collection:
/// pictures the gallery shows, videos, and everything else in Downloads.
#[cfg(target_os = "android")]
fn media_collection(mime: &str) -> (&'static str, &'static str) {
    match mime {
        m if m.starts_with("video/") => (
            "android/provider/MediaStore$Video$Media",
            "Movies/Rusty Painter",
        ),
        "image/png" | "image/jpeg" | "image/webp" | "image/gif" | "image/tiff" => (
            "android/provider/MediaStore$Images$Media",
            "Pictures/Rusty Painter",
        ),
        _ => (
            "android/provider/MediaStore$Downloads",
            "Download/Rusty Painter",
        ),
    }
}

/// Move the file at `path` into the shared storage (Pictures, Movies or
/// Download, under "Rusty Painter"), where other apps see it.
#[cfg(target_os = "android")]
pub fn publish_file(path: &std::path::Path, mime: &str) -> Result<AndroidExport, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Couldn't read the export: {e}"))?;
    let file_name = path
        .file_name()
        .map_or_else(|| "export".into(), |n| n.to_string_lossy().into_owned());
    let (collection_class, dir) = media_collection(mime);
    let result = with_android_env(|env, activity| {
        let resolver = content_resolver(env, &activity)?;
        let values = env
            .new_object("android/content/ContentValues", "()V", &[])
            .map_err(jerr)?;
        put_string(env, &values, "_display_name", &file_name)?;
        put_string(env, &values, "mime_type", mime)?;
        put_string(env, &values, "relative_path", dir)?;
        let collection = env
            .get_static_field(
                collection_class,
                "EXTERNAL_CONTENT_URI",
                "Landroid/net/Uri;",
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        let uri = env
            .call_method(
                &resolver,
                "insert",
                "(Landroid/net/Uri;Landroid/content/ContentValues;)Landroid/net/Uri;",
                &[JValue::Object(&collection), JValue::Object(&values)],
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        if uri.is_null() {
            return Err("Android refused to create the file".to_string());
        }
        let out = env
            .call_method(
                &resolver,
                "openOutputStream",
                "(Landroid/net/Uri;)Ljava/io/OutputStream;",
                &[JValue::Object(&uri)],
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        let array = env.byte_array_from_slice(&bytes).map_err(jerr)?;
        env.call_method(&out, "write", "([B)V", &[JValue::Object(&array)])
            .map_err(jerr)?;
        env.call_method(&out, "close", "()V", &[]).map_err(jerr)?;
        Ok(AndroidExport {
            message: format!("Saved to {dir}/{file_name}"),
            share_uri: Some(uri_to_string(env, &uri)?),
            share_mime: Some(mime.to_string()),
        })
    });
    let _ = std::fs::remove_file(path);
    result
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

/// Helper class `name` (like "FilePicker"), loaded from the embedded dex.
#[cfg(target_os = "android")]
fn helper_class<'a>(
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
fn picker_class<'a>(
    env: &mut jni::JNIEnv<'a>,
    activity: &JObject<'_>,
) -> Result<jni::objects::JClass<'a>, String> {
    helper_class(env, activity, "FilePicker")
}

/// Show or hide the soft keyboard (NativeActivity can't by itself).
#[cfg(target_os = "android")]
pub fn show_keyboard(on: bool) -> Result<(), String> {
    with_android_env(|env, activity| {
        let class = helper_class(env, &activity, "TextInput")?;
        env.call_static_method(
            &class,
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
/// was typed on it.
#[cfg(target_os = "android")]
pub fn keyboard_input(wanted: bool) -> Vec<eframe::egui::Event> {
    use std::sync::atomic::Ordering;
    if KEYBOARD_SHOWN.swap(wanted, Ordering::Relaxed) != wanted
        && let Err(err) = show_keyboard(wanted)
    {
        log::error!("Keyboard: {err}");
    }
    if wanted { typed() } else { Vec::new() }
}

/// What was typed on the soft keyboard since last asked.
#[cfg(target_os = "android")]
pub fn typed() -> Vec<eframe::egui::Event> {
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
                .call_static_method(&class, "poll", "()Ljava/lang/String;", &[])
                .and_then(|v| v.l())
                .map_err(jerr)?;
            if s.is_null() {
                return Ok(events);
            }
            let s: String = env.get_string(&JString::from(s)).map_err(jerr)?.into();
            match s.split_at(1) {
                ("t", text) => events.push(Event::Text(text.to_string())),
                (code, _) => {
                    let k = if code == "b" {
                        Key::Backspace
                    } else {
                        Key::Enter
                    };
                    events.extend([key(k, true), key(k, false)]);
                }
            }
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
            &class,
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
            &class,
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
            .call_static_method(&class, "poll", "()[Ljava/lang/String;", &[])
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

/// A picked file's name and contents.
#[cfg(target_os = "android")]
pub fn picker_read(uri: &str) -> Result<(String, Vec<u8>), String> {
    with_android_env(|env, activity| {
        let class = picker_class(env, &activity)?;
        let juri = env.new_string(uri).map_err(jerr)?;
        let name = env
            .call_static_method(
                &class,
                "displayName",
                "(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/String;",
                &[JValue::Object(&activity), JValue::Object(&juri)],
            )
            .and_then(|v| v.l())
            .map_err(jerr)?;
        let name: String = env.get_string(&JString::from(name)).map_err(jerr)?.into();
        let bytes = env
            .call_static_method(
                &class,
                "read",
                "(Landroid/content/Context;Ljava/lang/String;)[B",
                &[JValue::Object(&activity), JValue::Object(&juri)],
            )
            .and_then(|v| v.l())
            .map_err(|_| format!("Couldn't read {name}"))?;
        let bytes = env
            .convert_byte_array(jni::objects::JByteArray::from(bytes))
            .map_err(jerr)?;
        Ok((name, bytes))
    })
}

/// Write `bytes` into a file the picker created.
#[cfg(target_os = "android")]
pub fn picker_write(uri: &str, bytes: &[u8]) -> Result<(), String> {
    with_android_env(|env, activity| {
        let class = picker_class(env, &activity)?;
        let juri = env.new_string(uri).map_err(jerr)?;
        let array = env.byte_array_from_slice(bytes).map_err(jerr)?;
        env.call_static_method(
            &class,
            "write",
            "(Landroid/content/Context;Ljava/lang/String;[B)V",
            &[
                JValue::Object(&activity),
                JValue::Object(&juri),
                JValue::Object(&array),
            ],
        )
        .map_err(|_| "Couldn't write the file".to_string())?;
        Ok(())
    })
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

//! Android platform glue: JNI calls for the media store, sharing and
//! file access.

#[cfg(target_os = "android")]
use crate::project::export::{ExportFormat, encode_color_image};
#[cfg(target_os = "android")]
use eframe::egui::ColorImage;
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
    // next JNI call: clear it.
    if result.is_err() && env.exception_check().unwrap_or(false) {
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

#[cfg(target_os = "android")]
pub fn save_image_to_media_store(
    img: ColorImage,
    file_name: &str,
    format: ExportFormat,
) -> Result<AndroidExport, String> {
    let bytes = encode_color_image(img, format)?;
    let mime = format.mime_type();

    with_android_env(|env, activity| {
        let resolver = env
            .call_method(
                &activity,
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;

        let values = env
            .new_object("android/content/ContentValues", "()V", &[])
            .map_err(|e| e.to_string())?;
        put_string(env, &values, "_display_name", file_name)?;
        put_string(env, &values, "mime_type", mime)?;
        put_string(env, &values, "relative_path", "Pictures/Rusty Painter")?;

        let collection = env
            .get_static_field(
                "android/provider/MediaStore$Images$Media",
                "EXTERNAL_CONTENT_URI",
                "Landroid/net/Uri;",
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;

        let uri = env
            .call_method(
                &resolver,
                "insert",
                "(Landroid/net/Uri;Landroid/content/ContentValues;)Landroid/net/Uri;",
                &[JValue::Object(&collection), JValue::Object(&values)],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;

        if uri.is_null() {
            return Err("Android MediaStore insert returned null".to_string());
        }

        let output_stream = env
            .call_method(
                &resolver,
                "openOutputStream",
                "(Landroid/net/Uri;)Ljava/io/OutputStream;",
                &[JValue::Object(&uri)],
            )
            .map_err(|e| e.to_string())?
            .l()
            .map_err(|e| e.to_string())?;

        let byte_array = env
            .byte_array_from_slice(&bytes)
            .map_err(|e| e.to_string())?;
        env.call_method(
            &output_stream,
            "write",
            "([B)V",
            &[JValue::Object(&JObject::from(byte_array))],
        )
        .map_err(|e| e.to_string())?;
        env.call_method(&output_stream, "flush", "()V", &[])
            .map_err(|e| e.to_string())?;
        env.call_method(&output_stream, "close", "()V", &[])
            .map_err(|e| e.to_string())?;

        let uri_string = uri_to_string(env, &uri)?;
        Ok(AndroidExport {
            message: format!("Saved to Pictures/Rusty Painter/{file_name}"),
            share_uri: Some(uri_string),
            share_mime: Some(mime.to_string()),
        })
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

#[cfg(not(target_os = "android"))]
pub fn save_image_to_media_store(
    _img: eframe::egui::ColorImage,
    _file_name: &str,
    _format: crate::project::export::ExportFormat,
) -> Result<AndroidExport, String> {
    Err("Android export backend is unavailable on this platform".to_string())
}

#[cfg(not(target_os = "android"))]
pub fn share_uri(_uri: &str, _mime: &str, _title: &str) -> Result<(), String> {
    Err("Android share backend is unavailable on this platform".to_string())
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

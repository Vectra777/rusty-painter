#[cfg(target_os = "android")]
use crate::utils::exporter::{ExportFormat, encode_color_image};
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
    f(&mut env, activity)
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
        put_string(env, &values, "relative_path", "Pictures/Rust Dab Painter")?;

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

        let byte_array = env.byte_array_from_slice(&bytes).map_err(|e| e.to_string())?;
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
            message: format!("Saved to Pictures/Rust Dab Painter/{file_name}"),
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
    _format: crate::utils::exporter::ExportFormat,
) -> Result<AndroidExport, String> {
    Err("Android export backend is unavailable on this platform".to_string())
}

#[cfg(not(target_os = "android"))]
pub fn share_uri(_uri: &str, _mime: &str, _title: &str) -> Result<(), String> {
    Err("Android share backend is unavailable on this platform".to_string())
}

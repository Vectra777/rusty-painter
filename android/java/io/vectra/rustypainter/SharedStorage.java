package io.vectra.rustypainter;

import android.content.ContentValues;
import android.content.Context;
import android.net.Uri;
import android.provider.MediaStore;

/**
 * Files put in shared storage (Pictures, Movies, Download) through the
 * media store: created pending (other apps don't see a half-written file),
 * written by the Rust side through a file descriptor, then shown, or
 * removed if the write failed.
 */
public class SharedStorage {
    /** A new pending file (`kind` 0 a picture, 1 a video, else a download): its URI. */
    public static String create(Context c, int kind, String name, String mime, String dir)
            throws Exception {
        Uri collection = kind == 0 ? MediaStore.Images.Media.EXTERNAL_CONTENT_URI
                : kind == 1 ? MediaStore.Video.Media.EXTERNAL_CONTENT_URI
                : MediaStore.Downloads.EXTERNAL_CONTENT_URI;
        ContentValues values = new ContentValues();
        values.put(MediaStore.MediaColumns.DISPLAY_NAME, name);
        values.put(MediaStore.MediaColumns.MIME_TYPE, mime);
        values.put(MediaStore.MediaColumns.RELATIVE_PATH, dir);
        values.put(MediaStore.MediaColumns.IS_PENDING, 1);
        Uri uri = c.getContentResolver().insert(collection, values);
        if (uri == null) throw new Exception("Android refused to create the file");
        return uri.toString();
    }

    /** Written: other apps may see it now. */
    public static void finish(Context c, String uri) throws Exception {
        ContentValues values = new ContentValues();
        values.put(MediaStore.MediaColumns.IS_PENDING, 0);
        c.getContentResolver().update(Uri.parse(uri), values, null, null);
    }

    /** The write failed: the file goes. */
    public static void discard(Context c, String uri) {
        try {
            c.getContentResolver().delete(Uri.parse(uri), null, null);
        } catch (Exception e) {
            // Nothing more to do: the system clears pending files later.
        }
    }
}

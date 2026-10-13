package io.vectra.rustypainter;

import android.app.Activity;
import android.app.Application;
import android.app.Fragment;
import android.content.ClipData;
import android.content.ContentResolver;
import android.content.Context;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.provider.OpenableColumns;
import java.io.ByteArrayOutputStream;
import java.io.InputStream;
import java.io.OutputStream;

/**
 * The system file picker (Storage Access Framework) for the NativeActivity,
 * which can't receive activity results itself: a headless fragment starts
 * the picker and keeps the result for the Rust side to poll.
 *
 * Loaded at run time from a dex embedded in the library (cargo-apk packs no
 * Java), so the fragment must never be restored by class name: its state is
 * left out of the activity's saved state.
 */
public class FilePicker extends Fragment {
    private static final String TAG = "rusty_painter_picker";
    private static final String FRAGMENTS_KEY = "android:fragments";

    /** 0 nothing asked, 1 picker open, 2 answered. */
    private static volatile int state;
    /** The picked URIs; empty when cancelled. */
    private static volatile String[] picked = new String[0];
    private static boolean hooked;

    private Intent intent;

    public FilePicker() {}

    /** Pick existing files of the given MIME types. */
    public static void open(Activity activity, String[] mimes, boolean multiple) {
        Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        i.addCategory(Intent.CATEGORY_OPENABLE);
        i.setType(mimes.length == 1 ? mimes[0] : "*/*");
        if (mimes.length > 1) i.putExtra(Intent.EXTRA_MIME_TYPES, mimes);
        i.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, multiple);
        start(activity, i);
    }

    /** Pick where to create a file called `name`. */
    public static void create(Activity activity, String mime, String name) {
        Intent i = new Intent(Intent.ACTION_CREATE_DOCUMENT);
        i.addCategory(Intent.CATEGORY_OPENABLE);
        i.setType(mime);
        i.putExtra(Intent.EXTRA_TITLE, name);
        start(activity, i);
    }

    private static void start(final Activity activity, Intent i) {
        state = 1;
        picked = new String[0];
        final FilePicker f = new FilePicker();
        f.intent = i;
        activity.runOnUiThread(() -> {
            hook(activity);
            activity.getFragmentManager().beginTransaction().add(f, TAG).commitNowAllowingStateLoss();
        });
    }

    /** Keep the fragment out of saved state (see the class comment). */
    private static void hook(Activity activity) {
        if (hooked) return;
        hooked = true;
        activity.registerActivityLifecycleCallbacks(new Application.ActivityLifecycleCallbacks() {
            @Override public void onActivityPostSaveInstanceState(Activity a, Bundle out) {
                out.remove(FRAGMENTS_KEY);
            }
            @Override public void onActivityCreated(Activity a, Bundle b) {}
            @Override public void onActivityStarted(Activity a) {}
            @Override public void onActivityResumed(Activity a) {}
            @Override public void onActivityPaused(Activity a) {}
            @Override public void onActivityStopped(Activity a) {}
            @Override public void onActivitySaveInstanceState(Activity a, Bundle b) {}
            @Override public void onActivityDestroyed(Activity a) {}
        });
    }

    @Override
    public void onAttach(Context context) {
        super.onAttach(context);
        if (intent != null) {
            try {
                startActivityForResult(intent, 1);
            } catch (Exception e) {
                finish(new String[0]);
            }
            intent = null;
        }
    }

    @Override
    public void onActivityResult(int request, int result, Intent data) {
        String[] uris = new String[0];
        if (result == Activity.RESULT_OK && data != null) {
            ClipData clips = data.getClipData();
            if (clips != null) {
                uris = new String[clips.getItemCount()];
                for (int k = 0; k < uris.length; k++) uris[k] = clips.getItemAt(k).getUri().toString();
            } else if (data.getData() != null) {
                uris = new String[] {data.getData().toString()};
            }
        }
        finish(uris);
    }

    private void finish(String[] uris) {
        picked = uris;
        state = 2;
        if (getFragmentManager() != null) {
            getFragmentManager().beginTransaction().remove(this).commitAllowingStateLoss();
        }
    }

    /** null while the picker is open or nothing was asked; then the answer, once. */
    public static String[] poll() {
        if (state != 2) return null;
        state = 0;
        return picked;
    }

    public static boolean busy() {
        return state == 1;
    }

    public static String displayName(Context c, String uri) {
        try (Cursor cursor = c.getContentResolver().query(Uri.parse(uri),
                new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst()) return cursor.getString(0);
        } catch (Exception e) {
            // Fall through to the URI's last segment.
        }
        String last = Uri.parse(uri).getLastPathSegment();
        return last == null ? "file" : last;
    }

    public static byte[] read(Context c, String uri) throws Exception {
        try (InputStream in = c.getContentResolver().openInputStream(Uri.parse(uri))) {
            if (in == null) throw new Exception("Couldn't open " + uri);
            ByteArrayOutputStream out = new ByteArrayOutputStream();
            byte[] buf = new byte[1 << 16];
            for (int n; (n = in.read(buf)) > 0; ) out.write(buf, 0, n);
            return out.toByteArray();
        }
    }

    public static void write(Context c, String uri, byte[] data) throws Exception {
        ContentResolver r = c.getContentResolver();
        // "wt": truncate, should the picked file already have content.
        try (OutputStream out = r.openOutputStream(Uri.parse(uri), "wt")) {
            if (out == null) throw new Exception("Couldn't write " + uri);
            out.write(data);
        }
    }
}

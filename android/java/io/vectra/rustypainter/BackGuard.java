package io.vectra.rustypainter;

import android.app.Activity;
import android.os.Build;
import android.window.OnBackInvokedDispatcher;

/**
 * Back (the edge swipe most of all) would end the NativeActivity: on
 * Android 13+ the system takes it before the app sees a key. A callback
 * keeps the app open and hands Back to egui as Escape (closing a menu or
 * a window, cancelling a selection); Home still leaves the app.
 */
public class BackGuard {
    public static void install(final Activity activity) {
        if (Build.VERSION.SDK_INT < 33) return;
        activity.runOnUiThread(() -> activity.getOnBackInvokedDispatcher()
                .registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT,
                        () -> TextInput.queue("x")));
    }
}

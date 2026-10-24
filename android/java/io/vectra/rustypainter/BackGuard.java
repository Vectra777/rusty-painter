package io.vectra.rustypainter;

import android.app.Activity;
import android.os.Build;
import android.window.OnBackInvokedDispatcher;

/**
 * Back (the edge swipe most of all) would end the NativeActivity: on
 * Android 13+ the system takes it before the app sees a key. A callback
 * that does nothing keeps the app open; Home still leaves it.
 */
public class BackGuard {
    public static void install(final Activity activity) {
        if (Build.VERSION.SDK_INT < 33) return;
        activity.runOnUiThread(() -> activity.getOnBackInvokedDispatcher()
                .registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT, () -> {}));
    }
}

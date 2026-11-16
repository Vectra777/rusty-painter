package io.vectra.rustypainter;

import android.app.Activity;
import android.content.ComponentCallbacks2;
import android.content.res.Configuration;

/**
 * Tells the Rust side when memory runs low (through TextInput's queue,
 * which it polls every frame), so it can drop caches and autosave before
 * the system ends the app.
 */
public class MemoryWatch implements ComponentCallbacks2 {
    public static void install(Activity activity) {
        activity.registerComponentCallbacks(new MemoryWatch());
    }

    @Override
    public void onTrimMemory(int level) {
        // Running low, or in the background and on the list to be ended;
        // not just the app's UI going out of sight.
        if (level >= TRIM_MEMORY_RUNNING_LOW && level != TRIM_MEMORY_UI_HIDDEN) {
            TextInput.queue("m");
        }
    }

    @Override
    public void onLowMemory() {
        TextInput.queue("m");
    }

    @Override
    public void onConfigurationChanged(Configuration config) {}
}

package io.vectra.rustypainter;

import android.app.Activity;
import android.content.Context;
import android.text.InputType;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
import java.util.concurrent.ConcurrentLinkedQueue;

/**
 * The soft keyboard for the NativeActivity, whose own view isn't a text
 * editor (Android refuses to show the keyboard for it): a hidden view the
 * keyboard types into, its input queued for the Rust side to poll.
 *
 * Queued: "t" + text typed, "b" backspace, "e" enter.
 */
public class TextInput extends View {
    private static final ConcurrentLinkedQueue<String> typed = new ConcurrentLinkedQueue<>();
    private static TextInput view;

    public TextInput(Context context) {
        super(context);
        setFocusable(true);
        setFocusableInTouchMode(true);
    }

    @Override
    public boolean onCheckIsTextEditor() {
        return true;
    }

    @Override
    public InputConnection onCreateInputConnection(EditorInfo info) {
        info.inputType = InputType.TYPE_CLASS_TEXT;
        info.imeOptions = EditorInfo.IME_ACTION_DONE
                | EditorInfo.IME_FLAG_NO_EXTRACT_UI
                | EditorInfo.IME_FLAG_NO_FULLSCREEN;
        return new BaseInputConnection(this, false) {
            @Override
            public boolean commitText(CharSequence text, int newCursorPosition) {
                typed.add("t" + text);
                return true;
            }

            @Override
            public boolean deleteSurroundingText(int before, int after) {
                for (int i = 0; i < before; i++) typed.add("b");
                return true;
            }

            @Override
            public boolean sendKeyEvent(KeyEvent event) {
                if (event.getAction() != KeyEvent.ACTION_DOWN) return true;
                switch (event.getKeyCode()) {
                    case KeyEvent.KEYCODE_DEL:
                        typed.add("b");
                        break;
                    case KeyEvent.KEYCODE_ENTER:
                        typed.add("e");
                        break;
                    default:
                        int c = event.getUnicodeChar();
                        if (c != 0) typed.add("t" + new String(Character.toChars(c)));
                }
                return true;
            }

            @Override
            public boolean performEditorAction(int action) {
                typed.add("e");
                return true;
            }
        };
    }

    /** Show or hide the keyboard. */
    public static void show(final Activity activity, final boolean on) {
        activity.runOnUiThread(() -> {
            if (view == null) {
                view = new TextInput(activity);
                ViewGroup root = (ViewGroup) activity.getWindow().getDecorView();
                root.addView(view, new ViewGroup.LayoutParams(1, 1));
            }
            InputMethodManager imm = activity.getSystemService(InputMethodManager.class);
            if (on) {
                view.requestFocus();
                imm.restartInput(view);
                imm.showSoftInput(view, 0);
            } else {
                imm.hideSoftInputFromWindow(view.getWindowToken(), 0);
                view.clearFocus();
            }
        });
    }

    /** The next thing typed, or null. */
    public static String poll() {
        return typed.poll();
    }
}

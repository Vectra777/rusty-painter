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
 * Queued: "t" + text typed, "b" backspace, "d" delete (the character after
 * the cursor), "e" enter; and, from the other helpers, "x" Back (Escape)
 * and "m" memory running low.
 *
 * Keyboards that suggest words (Gboard, Samsung's, every CJK one) type a
 * word as "composing" text, replaced as it changes and then committed: it
 * is typed as it goes, each change sent as the backspaces and text that
 * turn the last into the new.
 */
public class TextInput extends View {
    private static final ConcurrentLinkedQueue<String> typed = new ConcurrentLinkedQueue<>();
    private static TextInput view;

    /** The composing text as typed so far (already in the text field). */
    private static String composing = "";

    public TextInput(Context context) {
        super(context);
        setFocusable(true);
        setFocusableInTouchMode(true);
    }

    /** Queue something for the Rust side (see the class comment). */
    static void queue(String what) {
        typed.add(what);
    }

    /** Turn the composing text typed so far into `text`. */
    private static void replaceComposing(String text) {
        int common = 0;
        int max = Math.min(composing.length(), text.length());
        while (common < max && composing.charAt(common) == text.charAt(common)) common++;
        // (Not between the halves of a surrogate pair.)
        if (common > 0 && Character.isHighSurrogate(composing.charAt(common - 1))) common--;
        int gone = composing.codePointCount(common, composing.length());
        for (int i = 0; i < gone; i++) typed.add("b");
        if (common < text.length()) typed.add("t" + text.substring(common));
        composing = text;
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
        composing = "";
        // Every edit is overridden: the base class's own handling would
        // send the text again as key events.
        return new BaseInputConnection(this, false) {
            @Override
            public boolean setComposingText(CharSequence text, int newCursorPosition) {
                replaceComposing(text.toString());
                return true;
            }

            @Override
            public boolean finishComposingText() {
                // Already typed: it simply stays.
                composing = "";
                return true;
            }

            @Override
            public boolean setComposingRegion(int start, int end) {
                // The text field's contents aren't known here: a word
                // already typed can't be taken up again.
                return false;
            }

            @Override
            public boolean commitText(CharSequence text, int newCursorPosition) {
                replaceComposing(text.toString());
                composing = "";
                return true;
            }

            @Override
            public boolean deleteSurroundingText(int before, int after) {
                composing = "";
                for (int i = 0; i < before; i++) typed.add("b");
                for (int i = 0; i < after; i++) typed.add("d");
                return true;
            }

            @Override
            public boolean deleteSurroundingTextInCodePoints(int before, int after) {
                return deleteSurroundingText(before, after);
            }

            @Override
            public boolean sendKeyEvent(KeyEvent event) {
                if (event.getAction() != KeyEvent.ACTION_DOWN) return true;
                composing = "";
                switch (event.getKeyCode()) {
                    case KeyEvent.KEYCODE_DEL:
                        typed.add("b");
                        break;
                    case KeyEvent.KEYCODE_FORWARD_DEL:
                        typed.add("d");
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
                composing = "";
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

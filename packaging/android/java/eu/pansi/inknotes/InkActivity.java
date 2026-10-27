package eu.pansi.inknotes;

import android.app.NativeActivity;
import android.content.Context;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.provider.OpenableColumns;
import android.text.InputType;
import android.view.View;
import android.view.ViewGroup;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;

/**
 * The app's activity (Issue #42): NativeActivity runs the Rust code (android_main in
 * libink_notes.so); this class adds what NativeActivity lacks.
 *
 * - Text input: NativeActivity's view offers keyboards no input connection, so they can only
 *   send key events, and characters without a key (umlauts chosen by long press, ...) got lost.
 *   An invisible view with its own input connection passes committed text to Rust.
 * - Picking files: the system file dialog (ACTION_OPEN_DOCUMENT); the chosen file is copied to
 *   the cache folder, as Rust can only open files by path.
 *
 * Rust calls showKeyboard and pickFile through JNI and receives the native* calls
 * (src/android_bridge.rs).
 */
public class InkActivity extends NativeActivity {
    static {
        // NativeActivity loads the library too, but without System.loadLibrary the native
        // methods below would not be found
        System.loadLibrary("ink_notes");
    }

    private static final int PICK_FILE = 1;

    static native void nativeCommitText(String text);

    static native void nativeDeleteSurroundingText(int before, int after);

    /** Path of the copied file, or null if nothing was chosen or copying failed. */
    static native void nativeFilePicked(String path);

    private TextSink textSink;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        textSink = new TextSink(this);
        addContentView(textSink, new ViewGroup.LayoutParams(1, 1));
    }

    /** Shows or hides the soft keyboard; called from Rust on any thread. */
    public void showKeyboard(final boolean show) {
        runOnUiThread(() -> {
            InputMethodManager imm =
                    (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
            if (show) {
                textSink.requestFocus();
                imm.restartInput(textSink);
                imm.showSoftInput(textSink, 0);
            } else {
                imm.hideSoftInputFromWindow(textSink.getWindowToken(), 0);
                textSink.clearFocus();
            }
        });
    }

    /** Opens the system file dialog for files of this MIME type ("image/*", "*" + "/*"). */
    public void pickFile(final String mimeType) {
        runOnUiThread(() -> {
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType(mimeType);
            try {
                startActivityForResult(intent, PICK_FILE);
            } catch (Exception e) {
                nativeFilePicked(null);
            }
        });
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != PICK_FILE) {
            return;
        }
        final Uri uri = (resultCode == RESULT_OK && data != null) ? data.getData() : null;
        if (uri == null) {
            nativeFilePicked(null);
            return;
        }
        // Copying a large file must not block the UI thread
        new Thread(() -> {
            String path = null;
            try {
                path = copyToCache(uri);
            } catch (Exception e) {
                android.util.Log.w("ink-notes", "copying the picked file failed", e);
            }
            nativeFilePicked(path);
        }).start();
    }

    private String copyToCache(Uri uri) throws Exception {
        String name = "import";
        try (Cursor cursor = getContentResolver().query(
                uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (cursor != null && cursor.moveToFirst() && !cursor.isNull(0)) {
                name = cursor.getString(0).replace('/', '_');
            }
        }
        File dir = new File(getCacheDir(), "picked");
        File[] old = dir.listFiles();
        if (old != null) {
            for (File f : old) {
                f.delete();
            }
        }
        dir.mkdirs();
        File target = new File(dir, name);
        try (InputStream in = getContentResolver().openInputStream(uri);
                OutputStream out = new FileOutputStream(target)) {
            byte[] buffer = new byte[64 * 1024];
            int n;
            while ((n = in.read(buffer)) > 0) {
                out.write(buffer, 0, n);
            }
        }
        return target.getAbsolutePath();
    }

    /** Invisible view that receives the keyboard's text. */
    private static class TextSink extends View {
        TextSink(Context context) {
            super(context);
            setFocusable(true);
            setFocusableInTouchMode(true);
        }

        @Override
        public boolean onCheckIsTextEditor() {
            return true;
        }

        @Override
        public InputConnection onCreateInputConnection(EditorInfo outAttrs) {
            // Visible password: keyboards commit every character right away instead of
            // composing words with suggestions, which the app could not show
            outAttrs.inputType = InputType.TYPE_CLASS_TEXT
                    | InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
                    | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
                    | InputType.TYPE_TEXT_FLAG_MULTI_LINE;
            outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI
                    | EditorInfo.IME_FLAG_NO_FULLSCREEN
                    | EditorInfo.IME_ACTION_NONE;
            return new TextConnection(this);
        }
    }

    /** Passes committed text and deletions to Rust; key events go the usual way. */
    private static class TextConnection extends BaseInputConnection {
        private String composing = "";

        TextConnection(View view) {
            super(view, false);
        }

        @Override
        public boolean commitText(CharSequence text, int newCursorPosition) {
            composing = "";
            if (text.length() > 0) {
                nativeCommitText(text.toString());
            }
            return true;
        }

        @Override
        public boolean setComposingText(CharSequence text, int newCursorPosition) {
            // Not shown while composing; taken over once finished
            composing = text.toString();
            return true;
        }

        @Override
        public boolean finishComposingText() {
            if (!composing.isEmpty()) {
                nativeCommitText(composing);
                composing = "";
            }
            return true;
        }

        @Override
        public boolean deleteSurroundingText(int beforeLength, int afterLength) {
            nativeDeleteSurroundingText(beforeLength, afterLength);
            return true;
        }
    }
}

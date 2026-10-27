//! Android: calls into the app's Java activity and what it reports back (Issue #42).
//!
//! `packaging/android/java/eu/pansi/inknotes/InkActivity.java` extends NativeActivity with
//! text input through an input connection (so that keyboards can send any character) and the
//! system file dialog. Rust calls its methods through JNI; it calls the `native*` functions
//! below, possibly on another thread, so they only queue messages and wake the event loop.

use std::path::PathBuf;
use std::sync::Mutex;

use jni::objects::{JObject, JString, JValue};
use jni::sys::jint;
use jni::JNIEnv;

use crate::file_picker::PickerKind;
use crate::ui::UiAction;

enum Message {
    Text(String),
    Delete { before: u32, after: u32 },
    Picked(Option<PathBuf>),
}

static MESSAGES: Mutex<Vec<Message>> = Mutex::new(Vec::new());
/// What the open file dialog is for
static PICKING: Mutex<Option<PickerKind>> = Mutex::new(None);

fn push(message: Message) {
    MESSAGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(message);
    if let Some(app) = crate::platform::android_app() {
        app.create_waker().wake();
    }
}

#[no_mangle]
pub extern "system" fn Java_eu_pansi_inknotes_InkActivity_nativeCommitText(
    mut env: JNIEnv,
    _class: JObject,
    text: JString,
) {
    if let Ok(text) = env.get_string(&text) {
        push(Message::Text(text.into()));
    }
}

#[no_mangle]
pub extern "system" fn Java_eu_pansi_inknotes_InkActivity_nativeDeleteSurroundingText(
    _env: JNIEnv,
    _class: JObject,
    before: jint,
    after: jint,
) {
    push(Message::Delete {
        before: before.max(0) as u32,
        after: after.max(0) as u32,
    });
}

#[no_mangle]
pub extern "system" fn Java_eu_pansi_inknotes_InkActivity_nativeFilePicked(
    mut env: JNIEnv,
    _class: JObject,
    path: JString,
) {
    let path = if path.is_null() {
        None
    } else {
        env.get_string(&path)
            .ok()
            .map(|p| PathBuf::from(String::from(p)))
    };
    push(Message::Picked(path));
}

/// Is a message waiting (then the app draws a new frame to take it)?
pub fn has_messages() -> bool {
    !MESSAGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_empty()
}

/// Typed text and deletions as egui input, and the action for a picked file.
pub fn take_messages() -> (Vec<egui::Event>, Vec<UiAction>) {
    let messages = std::mem::take(&mut *MESSAGES.lock().unwrap_or_else(|e| e.into_inner()));
    let mut events = Vec::new();
    let mut actions = Vec::new();
    for message in messages {
        match message {
            Message::Text(text) => events.extend(crate::osk::committed_text_events(&text)),
            Message::Delete { before, after } => {
                events.extend(crate::osk::deletion_key_events(before, after))
            }
            Message::Picked(path) => {
                let kind = PICKING.lock().unwrap_or_else(|e| e.into_inner()).take();
                match (kind, path) {
                    (Some(PickerKind::Image), Some(path)) => {
                        actions.push(UiAction::InsertImageFile(path))
                    }
                    (Some(PickerKind::Import), Some(path)) => {
                        actions.push(UiAction::ImportFile(path))
                    }
                    _ => {}
                }
            }
        }
    }
    (events, actions)
}

/// Runs `f` with the JNI environment and the activity; local references it creates are
/// freed afterwards.
fn with_activity(
    f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<()>,
) -> Result<(), String> {
    let app = crate::platform::android_app().ok_or("no activity")?;
    // Safety: pointers to the VM and the activity (a global reference) that live as long as
    // the app
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm
        .attach_current_thread_permanently()
        .map_err(|e| e.to_string())?;
    let activity = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
    let result = env.with_local_frame(16, |env| f(env, &activity));
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    result.map_err(|e| e.to_string())
}

pub fn show_keyboard(show: bool) {
    let result = with_activity(|env, activity| {
        env.call_method(
            activity,
            "showKeyboard",
            "(Z)V",
            &[JValue::Bool(show.into())],
        )
        .map(|_| ())
    });
    if let Err(err) = result {
        log::warn!("showKeyboard: {err}");
    }
}

/// Opens the system file dialog instead of the app's own; false if that is not possible
/// (e.g. for choosing a folder).
pub fn pick_file(kind: PickerKind) -> bool {
    let mime_type = match kind {
        PickerKind::Image => "image/*",
        // Xournal++, Rnote and Saber files have no MIME type of their own
        PickerKind::Import => "*/*",
        PickerKind::Folder => return false,
    };
    *PICKING.lock().unwrap_or_else(|e| e.into_inner()) = Some(kind);
    let result = with_activity(|env, activity| {
        let mime = env.new_string(mime_type)?;
        env.call_method(
            activity,
            "pickFile",
            "(Ljava/lang/String;)V",
            &[JValue::Object(&mime)],
        )
        .map(|_| ())
    });
    match result {
        Ok(()) => true,
        Err(err) => {
            log::warn!("pickFile: {err}");
            *PICKING.lock().unwrap_or_else(|e| e.into_inner()) = None;
            false
        }
    }
}

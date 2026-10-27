//! ink-notes: Latenzoptimierte Inking-App für Linux.

pub mod app;
pub mod assets;
pub mod autosave;
pub mod background;
pub mod canvas;
pub mod disk_watch;
pub mod document;
pub mod export;
pub mod file_picker;
pub mod filter;
pub mod geometry;
pub mod history;
pub mod i18n;
pub mod image_layer;
pub mod import;
pub mod library;
pub mod osk;
pub mod page_export;
pub mod paths;
pub mod pdf_doc;
pub mod pdf_merge;
pub mod pdf_writer;
pub mod platform;
pub mod rnote;
pub mod ruler;
pub mod saber;
pub mod selection;
pub mod settings;
pub mod spline;
pub mod storage;
pub mod text;
pub mod ui;
pub mod updater;
pub mod viewport;
pub mod xopp;

/// Entry point on Android (Issue #42), called by the native activity.
///
/// `scripts/package-android.sh` builds the library and packs it into an APK.
#[cfg(target_os = "android")]
#[no_mangle]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    use winit::platform::android::EventLoopBuilderExtAndroid;

    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("ink-notes"),
    );
    // Panics would otherwise only go to stderr, which Android discards
    std::panic::set_hook(Box::new(|info| log::error!("{info}")));
    if let Some(dir) = app.internal_data_path() {
        paths::set_app_dir(dir);
    }
    let event_loop = match winit::event_loop::EventLoopBuilder::new()
        .with_android_app(app)
        .build()
    {
        Ok(event_loop) => event_loop,
        Err(err) => {
            log::error!("event loop: {err}");
            return;
        }
    };
    if let Err(err) = app::run_with(event_loop) {
        log::error!("{err}");
    }
}

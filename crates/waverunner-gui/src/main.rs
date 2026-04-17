#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod commands;
mod state;

use state::AppState;

#[cfg(target_os = "linux")]
fn apply_linux_webkit_workarounds() {
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        // Set this before GTK/WebKit initialization to avoid the known blank-window
        // failure mode seen on some Linux GPU/driver stacks.
        unsafe {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
    }
}

fn main() {
    #[cfg(target_os = "linux")]
    apply_linux_webkit_workarounds();

    tracing_subscriber::fmt::init();

    tauri::Builder::default()
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            commands::connect_device,
            commands::disconnect_device,
            commands::replay_file,
            commands::tune,
            commands::set_gain,
            commands::set_sample_rate,
            commands::start_demod,
            commands::stop_demod,
            commands::enable_decoder,
            commands::disable_decoder,
            commands::start_record,
            commands::stop_record,
            commands::generate_capture_path,
            commands::inspect_capture,
            commands::list_recent_captures,
            commands::remove_capture,
            commands::update_capture_metadata,
            commands::list_bookmarks,
            commands::save_current_bookmark,
            commands::remove_bookmark,
            commands::get_available_devices,
            commands::get_available_decoders,
            commands::get_decoder_catalog,
            commands::list_profiles,
            commands::activate_profile,
            commands::activate_general_scan,
            commands::deactivate_mode,
            commands::measure_signal,
            commands::analyze_burst,
            commands::estimate_modulation,
            commands::compare_spectra,
            commands::capture_reference,
            commands::toggle_tracking,
            commands::export_data,
            commands::add_annotation,
            commands::export_timeline,
        ])
        .run(tauri::generate_context!())
        .expect("Error running WaveRunner GUI");
}

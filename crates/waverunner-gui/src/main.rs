#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod bridge;
mod commands;
mod state;

use state::AppState;

fn main() {
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
            commands::get_available_devices,
            commands::get_available_decoders,
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

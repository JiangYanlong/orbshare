mod commands;

use std::sync::Arc;

use commands::{
    abort_recording, append_recording_chunk, capture_stats, check_screen_capture_permission,
    connect_host_audio_monitor, create_room, disconnect_host_audio_monitor, finish_recording,
    force_stop_room, get_active_capture_source, get_active_room, get_active_room_password,
    get_attendance_stats, get_capture_source_thumbnail, get_chat_messages, get_connected_viewers,
    get_recording_status, guest_send_ice_candidate, guest_send_sdp_offer, join_room, leave_room,
    list_capture_sources, open_screen_recording_settings, remove_viewer,
    request_screen_capture_permission, reveal_recording, scan_rooms, send_host_chat_message,
    send_host_pointer, set_viewer_hand_raised, set_viewer_muted, start_recording, stop_room,
    switch_capture_source, AppState, SharedState,
};
use tokio::sync::Mutex;

/// Initialize tracing so that all `tracing::info!` / `error!` / `warn!` calls
/// are actually emitted (by default they are no-ops without a subscriber).
///
/// Output goes to BOTH:
///   - the platform temporary directory's `orbshare.log` (append mode)
///   - stderr (visible when running the binary directly in a terminal)
fn init_tracing() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new("info,orbshare=debug,orb_signaling=info,orb_webrtc=info,orb_capture=info")
    });

    // Use the platform temp directory and keep stderr logging available even
    // when a file cannot be created (for example due to endpoint policy).
    let log_path = std::env::temp_dir().join("orbshare.log");
    let file_layer = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok()
        .map(|file| {
            fmt::layer()
                .with_writer(std::sync::Mutex::new(file))
                .with_ansi(false)
                .with_target(false)
                .with_thread_ids(false)
                .with_thread_names(false)
                .with_file(false)
                .with_line_number(false)
        });

    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(true)
        .with_target(false)
        .with_file(false)
        .with_line_number(false);

    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init()
        .ok();
}

pub fn run() {
    init_tracing();

    tracing::info!("=== OrbShare starting ===");

    let state: SharedState = Arc::new(Mutex::new(AppState::new()));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            create_room,
            get_active_room,
            get_active_room_password,
            get_connected_viewers,
            get_attendance_stats,
            get_chat_messages,
            send_host_chat_message,
            send_host_pointer,
            remove_viewer,
            set_viewer_muted,
            set_viewer_hand_raised,
            connect_host_audio_monitor,
            disconnect_host_audio_monitor,
            start_recording,
            append_recording_chunk,
            finish_recording,
            abort_recording,
            get_recording_status,
            reveal_recording,
            stop_room,
            force_stop_room,
            scan_rooms,
            join_room,
            leave_room,
            guest_send_sdp_offer,
            guest_send_ice_candidate,
            list_capture_sources,
            get_capture_source_thumbnail,
            get_active_capture_source,
            switch_capture_source,
            capture_stats,
            check_screen_capture_permission,
            request_screen_capture_permission,
            open_screen_recording_settings,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if nekoapi_widget_lib::handle_linux_toggle_request() {
        return;
    }
    nekoapi_widget_lib::run();
}

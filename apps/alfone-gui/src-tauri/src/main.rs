// Hide the console on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    alfone_gui_lib::run()
}

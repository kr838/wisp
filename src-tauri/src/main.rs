// Hide the console window on Windows. Without this the GNU toolchain links the
// binary as a console-subsystem app, so launching it pops a black terminal window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    selection_assistant_lib::run()
}

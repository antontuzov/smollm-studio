//! Desktop entry point. All logic lives in the library crate so it stays testable.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    smollm_studio_lib::run();
}

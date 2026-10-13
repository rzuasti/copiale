//! Copiale: a fast, minimalist native desktop client for Signal.

fn main() {
    copiale_core::logging::init();
    copiale_ui::run();
}

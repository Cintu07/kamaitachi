//! kamaitachi, the window browser. Built as a windows program, so no console opens
//! next to it.
#![windows_subsystem = "windows"]

fn main() {
    let urls: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = hayate::gui::run(urls) {
        let _ = std::fs::write(std::env::temp_dir().join("kamaitachi-error.txt"), e);
        std::process::exit(1);
    }
}

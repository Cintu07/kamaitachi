//! Every row of the hard-numbers table, as a test. Memory and timing budgets are only
//! meaningful in an optimized build: run `cargo test --release --test budgets`.

#[test]
fn hard_numbers_hold() {
    if cfg!(debug_assertions) {
        eprintln!("skipped: budgets are judged on release builds (cargo test --release --test budgets)");
        return;
    }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_hayate")).args(["bench", "--quick"]).output().expect("run hayate bench");
    let text = String::from_utf8_lossy(&out.stdout);
    eprintln!("{text}");
    assert!(out.status.success(), "a budget failed:\n{text}\n{}", String::from_utf8_lossy(&out.stderr));
}

//! Codex Meter backend scaffold.
//!
//! Phase 0 deliberately contains no business implementation. The executable
//! exists so the Rust crate and its test harness are wired before Phase 1.

fn main() {
    println!("codex-meter scaffold");
}

#[cfg(test)]
mod tests {
    #[test]
    fn phase_zero_scaffold_is_present() {
        assert_eq!(env!("CARGO_PKG_NAME"), "codex-meter");
    }
}

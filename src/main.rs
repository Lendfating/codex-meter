//! Codex Meter backend entry point.
//!
//! Phase 1 establishes the domain and storage layers. The long-running
//! collectors and HTTP service are intentionally deferred to later phases.

fn main() {
    println!("codex-meter storage foundation");
}

#[cfg(test)]
mod tests {
    #[test]
    fn crate_is_present() {
        assert_eq!(env!("CARGO_PKG_NAME"), "codex-meter");
    }
}

//! Compile-time vectorman version for `--version` / clap.
//!
//! Resolution order in `build.rs`:
//! 1. `VECTORMAN_VERSION` (set by `packaging/build-package.sh` / release CI)
//! 2. `git describe --tags --always --dirty`
//! 3. `CARGO_PKG_VERSION`
//!
//! A leading `v` is stripped so `v1.0.5` and `1.0.5` print the same.

pub const VERSION: &str = include_str!(concat!(env!("OUT_DIR"), "/version.txt"));

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_is_nonempty_single_line() {
        assert!(!VERSION.is_empty());
        assert!(!VERSION.contains('\n'));
        assert!(!VERSION.starts_with('v'));
    }
}

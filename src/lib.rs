//! kertweaker: pick kernel options in a GUI, then build that kernel for the
//! machine you are sitting at.
//!
//! The crate is deliberately dependency-free. Anything that genuinely needs a
//! big implementation is delegated to a tool a kernel build already requires:
//! `make` for kbuild, `tar` for tarballs, `gzip` for `/proc/config.gz`, and
//! `curl`/`wget` for downloads.

pub mod catalog;
pub mod json;
pub mod kconfig;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

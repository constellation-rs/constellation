//! A template Rust library crate.
//!
//! **[Crates.io](https://crates.io/crates/template-rust) │ [Repo](https://github.com/alecmocatta/template-rust)**
//!
//! This is template for Rust libraries, comprising a [`hello_world()`] function.
//!
//! # Example
//!
//! ```
//! use template_rust::hello_world;
//!
//! hello_world();
//! // prints: Hello, world!
//! ```
//!
//! # Note
//!
//! Caveat emptor.

#![doc(html_root_url = "https://docs.rs/template-rust/0.1.0")]
#![warn(
	missing_copy_implementations,
	missing_debug_implementations,
	missing_docs,
	trivial_casts,
	trivial_numeric_casts,
	unused_import_braces,
	unused_qualifications,
	unused_results,
	clippy::pedantic
)] // from https://github.com/rust-unofficial/patterns/blob/master/anti_patterns/deny-warnings.md
#![allow()]

/// Print "Hello, world!".
///
/// # Example
///
/// ```
/// use template_rust::hello_world;
///
/// hello_world();
/// // prints: Hello, world!
/// ```
///
/// # Panics
///
/// Will panic if printing fails.
pub fn hello_world() {
	print!("Hello, world!");
}

#[cfg(test)]
mod tests {
	use super::hello_world;

	#[test]
	fn succeeds() {
		hello_world();
	}
}

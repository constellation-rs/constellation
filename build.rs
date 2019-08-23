use std::env::{var, vars};

fn main() {
	println!("cargo:rerun-if-changed=build.rs");

	write("TARGET", &var("TARGET").unwrap());
	write("PROFILE", &var("PROFILE").unwrap());
	let features = vars()
		.filter(|(key, _)| key.starts_with("CARGO_FEATURE_"))
		.map(|(key, _)| key[14..].to_lowercase())
		.collect::<Vec<String>>()
		.join(" ");
	write("FEATURES", &features);
}
fn write(key: &str, value: &str) {
	println!("cargo:rustc-env={}={}", key, value);
}

[package]
name = "uniffi-bindgen"
version = "0.1.0"
edition = "2021"
publish = false
[workspace]
[dependencies]
uniffi = { version = "=0.32.2", features = ["cli", "cargo-metadata"] }

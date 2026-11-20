[package]
name = "{{crate-name}}"
version = "0.1.0"
edition = "2021"
publish = false
[workspace]
[lib]
crate-type = ["rlib", "cdylib"]
[dependencies]
arc-core = { {{arc-core-source}} }
arc-stack = { {{arc-stack-source}}, default-features = false, features = ["sqlite"] }
arc-ffi = { {{arc-ffi-source}} }
uniffi = { version = "=0.32.2", default-features = false }
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
uuid = { version = "1", features = ["v4"] }
diesel = { version = "2.2.6", features = ["sqlite"] }
# Android has no system SQLite to link: compile it from the bundled source.
libsqlite3-sys = { version = ">=0.30.1, <0.41.0", features = ["bundled"] }
tokio = { version = "1", features = ["rt"] }
[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tempfile = "3"

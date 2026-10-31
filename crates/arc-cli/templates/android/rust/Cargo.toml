[package]
name = "{{crate-name}}"
version = "0.1.0"
edition = "2021"
publish = false
[workspace]
[lib]
crate-type = ["rlib", "cdylib"]
[dependencies]
arc-core = { version = "{{arc-version}}", path = {{arc-core-path}} }
arc-stack = { version = "{{arc-version}}", path = {{arc-stack-path}}, default-features = false, features = ["sqlite"] }
arc-ffi = { version = "{{arc-version}}", path = {{arc-ffi-path}} }
uniffi = { version = "=0.32.2", default-features = false }
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
uuid = { version = "1", features = ["v4"] }
sqlx = { version = "0.8.6", default-features = false, features = ["sqlite", "runtime-tokio-rustls"] }
[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tempfile = "3"

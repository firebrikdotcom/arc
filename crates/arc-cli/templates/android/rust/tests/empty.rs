#[tokio::test]
async fn native_core_matches_registered_resource_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite");
    let manifest: serde_json::Value = serde_json::from_str(include_str!("../../.arc/android.json")).unwrap();
    let has_resources = !manifest["resources"].as_array().unwrap().is_empty();
    let _core = {{crate-name}}::ffi::NativeCore::open(path.to_str().unwrap().into()).await.unwrap();
    assert_eq!(path.exists(), has_resources);
}

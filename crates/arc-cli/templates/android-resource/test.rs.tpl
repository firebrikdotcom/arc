use {{crate-name}}::{ffi::NativeCore, domain::{{module}}::{aggregate::{{Type}}Aggregate, commands::{{Type}}Command, ffi::{self, {{Type}}ResourceError}}};
use arc_core::command_bus::CommandContext;
use arc_ffi::{CoreRegistry, FfiRuntime};
use std::sync::Arc;

#[test]
fn schema_initializer_and_native_open_are_send() {
    fn assert_send(_: impl Send) {}
    // Type-check the futures without executing them or creating a database.
    assert_send({{crate-name}}::schema::initialize("not-opened.sqlite"));
    assert_send(NativeCore::open("not-opened.sqlite".into()));
}

#[tokio::test]
async fn typed_crud_reopen_validation_and_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("app.sqlite").to_str().unwrap().to_owned();
    let native = NativeCore::open(path.clone()).await.unwrap();
    assert!(matches!(native.create_{{module}}(" \t".into()).await, Err({{Type}}ResourceError::EmptyName)));
    assert!(matches!(native.create_{{module}}("é".repeat(513)).await, Err({{Type}}ResourceError::NameTooLong)));
    let id = native.create_{{module}}("First".into()).await.unwrap();
    assert_eq!(native.get_{{module}}(id.clone()).await.unwrap().version, 1);
    assert!(native.list_{{view}}().await.unwrap().iter().any(|r| r.id == id));
    assert!(matches!(native.rename_{{module}}(id.clone(), " ".into()).await, Err({{Type}}ResourceError::EmptyName)));
    assert_eq!(native.get_{{module}}(id.clone()).await.unwrap().name, "First");
    native.rename_{{module}}(id.clone(), "Updated".into()).await.unwrap();
    let reopened = NativeCore::open(path.clone()).await.unwrap();
    assert_eq!(reopened.get_{{module}}(id.clone()).await.unwrap().name, "Updated");
    let registry = CoreRegistry::process().unwrap();
    let a = ffi::open(registry.clone(), path.clone()).await.unwrap();
    let b = ffi::open(registry, path.clone()).await.unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    let existing_id = id.clone();
    let error = a.run(move |s| async move {
        s.command_bus.dispatch({{Type}}Command::Create { id: existing_id, name: "Duplicate".into() }, CommandContext::for_actor("test"))
            .await.map_err({{Type}}ResourceError::from)?;
        Ok::<_, {{Type}}ResourceError>(())
    }).await.unwrap_err();
    assert!(matches!(error, {{Type}}ResourceError::AlreadyExists));
    let fresh = ffi::open(CoreRegistry::new(FfiRuntime::new().unwrap()), path.clone()).await.unwrap();
    assert_eq!(ffi::get(&fresh, id.clone()).await.unwrap().version, 2);
    let stored = id.clone();
    let journal = fresh.run(move |_| async move {
        use sqlx::Connection;
        let options = sqlx::sqlite::SqliteConnectOptions::new().filename(path);
        let mut db = sqlx::SqliteConnection::connect_with(&options).await.unwrap();
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode").fetch_one(&mut db).await.unwrap();
        db.close().await.unwrap();
        Ok::<_, {{Type}}ResourceError>(journal)
    }).await.unwrap();
    assert_eq!(journal, "delete");
    assert!(matches!(native.get_{{module}}("missing".into()).await, Err({{Type}}ResourceError::NotFound)));
    assert!(matches!(native.rename_{{module}}("missing".into(), "name".into()).await, Err({{Type}}ResourceError::NotFound)));
    assert!(matches!(native.delete_{{module}}("missing".into()).await, Err({{Type}}ResourceError::NotFound)));
    native.delete_{{module}}(stored.clone()).await.unwrap();
    assert!(matches!(reopened.get_{{module}}(stored).await, Err({{Type}}ResourceError::NotFound)));
    let _ = std::any::TypeId::of::<{{Type}}Aggregate>();
}

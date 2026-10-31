use arc_ffi::{CoreKey, FfiError, FfiRuntime};
use std::{path::PathBuf, sync::Arc};

#[test]
fn spawn_from_os_threads_uses_one_owned_worker() {
    let runtime = FfiRuntime::new().unwrap();
    let thread_runtime = runtime.clone();
    let owned_thread = std::thread::spawn(move || {
        let caller = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        caller
            .block_on(thread_runtime.run(async { std::thread::current().id() }))
            .unwrap()
    })
    .join()
    .unwrap();
    let caller = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    assert_eq!(
        owned_thread,
        caller
            .block_on(runtime.run(async { std::thread::current().id() }))
            .unwrap()
    );
}

#[tokio::test]
async fn nested_context_panic_cancellation_and_nonblocking_drop() {
    let runtime = FfiRuntime::new().unwrap();
    let caller_thread = std::thread::current().id();
    assert_ne!(
        caller_thread,
        runtime
            .run(async { std::thread::current().id() })
            .await
            .unwrap()
    );
    assert_eq!(
        runtime
            .run(async {
                panic!("private panic");
            })
            .await,
        Err(FfiError::OperationPanicked)
    );
    assert_eq!(
        runtime
            .run(async { Err::<(), _>("domain rejection") })
            .await
            .unwrap(),
        Err("domain rejection")
    );
    drop(runtime);
    // Mapping a cancelled Tokio join uses the same runtime boundary code.
}

#[test]
fn sqlite_identity_normalizes_new_existing_and_symlink_paths() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("db.sqlite");
    let new = CoreKey::sqlite_file(file.clone()).unwrap();
    std::fs::write(&file, "").unwrap();
    assert_eq!(
        new,
        CoreKey::sqlite_file(dir.path().join("./db.sqlite")).unwrap()
    );
    assert_eq!(new.sqlite_path(), Some(file.as_path()));
    let colon = dir.path().join("notes:v2.sqlite");
    assert_eq!(
        CoreKey::sqlite_file(colon.clone()).unwrap().sqlite_path(),
        Some(colon.as_path())
    );
    #[cfg(unix)]
    {
        let link = dir.path().join("alias");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(new, CoreKey::sqlite_file(link).unwrap());
        let dangling = dir.path().join("dangling");
        std::os::unix::fs::symlink(dir.path().join("absent"), &dangling).unwrap();
        assert_eq!(
            CoreKey::sqlite_file(dangling),
            Err(FfiError::InvalidDatabasePath)
        );
        use std::os::unix::ffi::OsStringExt;
        assert_eq!(
            CoreKey::sqlite_file(PathBuf::from(std::ffi::OsString::from_vec(vec![255]))),
            Err(FfiError::InvalidDatabasePath)
        );
    }
    for path in [
        dir.path().to_path_buf(),
        dir.path().join("missing/db"),
        PathBuf::from(":memory:"),
        PathBuf::from("file:db.sqlite"),
        PathBuf::from("sqlite://db.sqlite"),
        PathBuf::from(""),
    ] {
        assert_eq!(
            CoreKey::sqlite_file(path),
            Err(FfiError::InvalidDatabasePath)
        );
    }
    assert_eq!(
        CoreKey::postgres(" ".into()),
        Err(FfiError::InvalidConfiguration)
    );
    assert!(CoreKey::postgres("stable-db".into())
        .unwrap()
        .sqlite_path()
        .is_none());
}

#[test]
fn process_registry_is_retained() {
    assert!(Arc::ptr_eq(
        &arc_ffi::CoreRegistry::process().unwrap(),
        &arc_ffi::CoreRegistry::process().unwrap()
    ));
}

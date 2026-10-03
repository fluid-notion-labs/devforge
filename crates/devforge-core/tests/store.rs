use devforge_core::state::{ServiceState, Transition, TransitionCause};
use devforge_core::store::Store;

#[tokio::test]
async fn store_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.sqlite3");
    let store = Store::open(&path).await.unwrap();

    let t = Transition {
        service: "web".into(),
        from: ServiceState::Idle,
        to: ServiceState::Up,
        cause: TransitionCause::ProviderPattern {
            pattern: "ready in 300ms".into(),
        },
        at_unix_ms: 1_700_000_000_000,
    };
    store.record_transition(&t).await.unwrap();

    let state = store.current_state().await.unwrap();
    assert_eq!(state.len(), 1);
    assert_eq!(state[0].service, "web");
    assert_eq!(state[0].state, ServiceState::Up);

    store
        .append_log("web".into(), "ready in 300 ms".into(), 1_700_000_000_500)
        .await
        .unwrap();

    let logs = store.service_logs("web".into(), 10).await.unwrap();
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].line, "ready in 300 ms");

    // reopening applies no migrations and keeps rows
    drop(store);
    let store2 = Store::open(&path).await.unwrap();
    assert_eq!(
        store2.current_state().await.unwrap()[0].state,
        ServiceState::Up
    );
}

#[tokio::test]
async fn store_upsert_replaces_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("s.sqlite3").as_path())
        .await
        .unwrap();
    for to in [
        ServiceState::Starting,
        ServiceState::Up,
        ServiceState::Compiling,
    ] {
        store
            .record_transition(&Transition {
                service: "api".into(),
                from: ServiceState::Idle,
                to,
                cause: TransitionCause::UserRequest,
                at_unix_ms: 1,
            })
            .await
            .unwrap();
    }
    let rows = store.current_state().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, ServiceState::Compiling);
    let logs = store.service_logs("api".into(), 5).await.unwrap();
    assert_eq!(logs.len(), 0, "transitions are not log lines");
}

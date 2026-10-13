use super::*;
#[tokio::test]
async fn simultaneous_first_clients_create_one_starter_and_never_relaunch_ended_work() {
    let (state, directory) = state("/bin/sh");
    let a = state.bootstrap(1, vec![]).await.unwrap();
    let b = state.bootstrap(1, vec![]).await.unwrap();
    let geo = Geometry::new(54, 23).unwrap();
    let (a, b) = tokio::join!(
        state.execute(a.token, Request::EnsureDefault { geometry: geo }),
        state.execute(b.token, Request::EnsureDefault { geometry: geo })
    );
    a.unwrap();
    b.unwrap();
    {
        let inner = state.inner.lock().await;
        assert_eq!(inner.model.spaces().count(), 1);
        assert_eq!(inner.model.tabs().count(), 1);
        assert_eq!(inner.terminals.len(), 1);
    }
    state.shutdown().await;
    drop(state);
    let state = State::load(directory.clone(), "/bin/sh".into()).unwrap();
    let boot = state.bootstrap(1, vec![]).await.unwrap();
    state
        .execute(boot.token, Request::EnsureDefault { geometry: geo })
        .await
        .unwrap();
    {
        let inner = state.inner.lock().await;
        assert_eq!(inner.model.tabs().count(), 1);
        assert!(
            inner
                .model
                .tabs()
                .all(|t| matches!(t.terminal, TerminalState::Ended(EndReason::ServerRestart)))
        );
        assert!(inner.terminals.is_empty());
    }
    std::fs::remove_dir_all(directory).unwrap();
}
#[tokio::test]
async fn workspace_creation_and_rename_are_durable_without_changing_runtime_identity() {
    let (state, directory) = state("/bin/sh");
    let boot = state.bootstrap(1, vec![]).await.unwrap();
    let Response::Space(source) = state
        .execute(
            boot.token,
            Request::CreateSpace {
                name: "Source".into(),
                directory: std::env::temp_dir(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("source");
    };
    let Response::Tab(tab) = state
        .execute(
            boot.token,
            Request::CreateWorkspace {
                source: Some(source.id),
                geometry: Geometry::new(54, 23).unwrap(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("tab");
    };
    assert_eq!(tab.directory, std::env::temp_dir());
    state
        .execute(
            boot.token,
            Request::RenameSpace {
                space: tab.space,
                name: "New name".into(),
            },
        )
        .await
        .unwrap();
    state
        .execute(
            boot.token,
            Request::RenameTab {
                tab: tab.id,
                label: "Agent work".into(),
            },
        )
        .await
        .unwrap();
    {
        let inner = state.inner.lock().await;
        assert_eq!(inner.model.tab(tab.id).unwrap().terminal, tab.terminal);
    }
    state.shutdown().await;
    drop(state);
    let state = State::load(directory.clone(), "/bin/sh".into()).unwrap();
    {
        let inner = state.inner.lock().await;
        assert_eq!(inner.model.tab(tab.id).unwrap().label, "Agent work");
        assert_eq!(
            inner
                .model
                .spaces()
                .find(|s| s.id == tab.space)
                .unwrap()
                .name,
            "New name"
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
}
#[tokio::test]
async fn starter_persistence_failure_cannot_launch_or_commit_partial_organization() {
    let (state, directory) = state("/bin/sh");
    let boot = state.bootstrap(1, vec![]).await.unwrap();
    std::fs::remove_dir(directory).unwrap();
    assert!(
        state
            .execute(
                boot.token,
                Request::EnsureDefault {
                    geometry: Geometry::new(54, 23).unwrap()
                }
            )
            .await
            .is_err()
    );
    let inner = state.inner.lock().await;
    assert_eq!(inner.model.spaces().count(), 0);
    assert_eq!(inner.model.tabs().count(), 0);
    assert!(inner.terminals.is_empty());
}

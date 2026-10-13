use super::*;
mod defaults;

fn state(shell: &str) -> (Arc<State>, PathBuf) {
    let directory = std::env::temp_dir().join(format!("sheepdog-state-{}", rand::random::<u64>()));
    std::fs::create_dir(&directory).unwrap();
    (
        State::load(directory.clone(), shell.into()).unwrap(),
        directory,
    )
}

#[tokio::test]
async fn expired_credentials_are_not_refreshed_by_authentication() {
    let (state, directory) = state("/bin/sh");
    let bootstrap = state.bootstrap(1, vec![]).await.unwrap();
    state
        .inner
        .lock()
        .await
        .clients
        .get_mut(&credential_key(bootstrap.token))
        .unwrap()
        .seen -= IDLE;
    assert!(state.authenticate(bootstrap.token).await.is_err());
    state.prune().await;
    assert!(state.authenticate(bootstrap.token).await.is_err());
    state.bootstrap(1, vec![]).await.unwrap();
    assert_eq!(state.inner.lock().await.clients.len(), 1);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn unknown_credentials_cannot_mutate_organization() {
    let (state, directory) = state("/bin/sh");
    assert!(
        state
            .execute(
                [0; 32],
                Request::CreateSpace {
                    name: "unauthorized".into(),
                    directory: std::env::temp_dir()
                }
            )
            .await
            .is_err()
    );
    assert_eq!(state.inner.lock().await.model.spaces().count(), 0);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn failed_persistence_does_not_commit_the_staged_model() {
    let (state, directory) = state("/bin/sh");
    let bootstrap = state.bootstrap(1, vec![]).await.unwrap();
    std::fs::remove_dir(&directory).unwrap();
    assert!(
        state
            .execute(
                bootstrap.token,
                Request::CreateSpace {
                    name: "not-saved".into(),
                    directory: std::env::temp_dir()
                }
            )
            .await
            .is_err()
    );
    assert_eq!(state.inner.lock().await.model.spaces().count(), 0);
}

#[tokio::test]
async fn failed_shell_launch_is_an_ended_tab_not_a_running_process() {
    let (state, directory) = state("/nonexistent/sheepdog-shell");
    let bootstrap = state.bootstrap(1, vec![]).await.unwrap();
    let Response::Space(space) = state
        .execute(
            bootstrap.token,
            Request::CreateSpace {
                name: "test".into(),
                directory: std::env::temp_dir(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("Space");
    };
    let Response::Tab(tab) = state
        .execute(
            bootstrap.token,
            Request::CreateTab {
                space: space.id,
                label: "shell".into(),
                geometry: Geometry::new(80, 24).unwrap(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("Tab");
    };
    assert!(matches!(
        tab.terminal,
        TerminalState::Ended(EndReason::LaunchFailed(_))
    ));
    assert!(state.inner.lock().await.terminals.is_empty());
    std::fs::remove_dir_all(directory).unwrap();
}

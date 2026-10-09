use sheepdog_client::{Client, View, ViewStatus};
use sheepdog_core::{Geometry, TabId, TerminalState};
use sheepdog_proto::{Request, Response};
use sheepdog_server::{Daemon, bootstrap};
use std::path::PathBuf;
use std::time::Duration;

struct Harness {
    directory: PathBuf,
    stopped: std::sync::Arc<tokio::sync::Notify>,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}
impl Harness {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("sheepdog-native-client-{}", rand::random::<u64>()));
        let daemon = Daemon::bind(
            "127.0.0.1:0".parse().unwrap(),
            directory.clone(),
            "/bin/sh".into(),
        )
        .unwrap();
        let stopped = std::sync::Arc::new(tokio::sync::Notify::new());
        let task = tokio::spawn(daemon.serve(stopped.clone()));
        Self {
            directory,
            stopped,
            task,
        }
    }
    async fn client(&self) -> std::sync::Arc<Client> {
        Client::connect(
            "127.0.0.1".into(),
            bootstrap(self.directory.clone()).await.unwrap(),
        )
        .await
        .unwrap()
    }
    async fn close(self) {
        self.stopped.notify_one();
        self.task.await.unwrap().unwrap();
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}
async fn tab(client: &Client) -> TabId {
    let Response::Space(space) = client
        .request(Request::CreateSpace {
            name: "Native".into(),
            directory: std::env::temp_dir(),
        })
        .await
        .unwrap()
    else {
        panic!("space");
    };
    let Response::Tab(tab) = client
        .request(Request::CreateTab {
            space: space.id,
            label: "Shell".into(),
            geometry: Geometry::new(80, 24).unwrap(),
        })
        .await
        .unwrap()
    else {
        panic!("tab");
    };
    tab.id
}
fn text(view: &View) -> String {
    let snapshot = view.snapshot();
    let Some(frame) = snapshot.frame else {
        return String::new();
    };
    let mut output = String::new();
    for row in 0..frame.rows() {
        for col in 0..frame.cols() {
            output.push_str(frame.cell_content(row, col));
        }
        output.push('\n');
    }
    output
}
async fn wait(view: &View, predicate: impl Fn(&View) -> bool) {
    let mut updates = view.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !predicate(view) {
            updates.changed().await.unwrap();
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "view did not reach expected state: {:?}, text {:?}",
            view.snapshot().status,
            text(view)
        )
    });
}

#[tokio::test]
async fn portable_client_preserves_sequences_across_pause_navigation_and_resume() {
    let harness = Harness::new();
    let client = harness.client().await;
    let id = tab(&client).await;
    let view = client
        .attach(id, Geometry::new(80, 24).unwrap(), false)
        .await
        .unwrap();
    wait(&view, |v| {
        v.snapshot().status == ViewStatus::Connected && v.snapshot().frame.is_some()
    })
    .await;
    view.input(b"stty -echo; printf '\\122\\105\\101\\104\\131\\n'\n".to_vec())
        .unwrap();
    wait(&view, |v| {
        text(v).contains("READY") && v.snapshot().unacked_bytes == 0
    })
    .await;
    view.input(b"printf 'NATIVE_FIRST\\n'\n".to_vec()).unwrap();
    wait(&view, |v| {
        text(v).contains("NATIVE_FIRST") && v.snapshot().unacked_bytes == 0
    })
    .await;
    view.pause();
    wait(&view, |v| v.snapshot().status == ViewStatus::Paused).await;
    assert!(view.input(b"wrong hidden Tab".to_vec()).is_err());
    let resumed = client
        .attach(id, Geometry::new(45, 12).unwrap(), false)
        .await
        .unwrap();
    wait(&resumed, |v| {
        v.snapshot().status == ViewStatus::Connected
            && v.snapshot().frame.as_ref().is_some_and(|f| f.cols() == 45)
    })
    .await;
    resumed
        .input(b"printf 'NATIVE_SECOND\\n'\n".to_vec())
        .unwrap();
    wait(&resumed, |v| {
        text(v).contains("NATIVE_SECOND") && v.snapshot().unacked_bytes == 0
    })
    .await;
    let output = text(&resumed);
    assert_eq!(output.matches("NATIVE_FIRST").count(), 1);
    assert_eq!(output.matches("NATIVE_SECOND").count(), 1);
    assert!(
        client
            .attach(id, Geometry::new(45, 12).unwrap(), true)
            .await
            .is_err(),
        "never silently change a requested Never policy into adaptive prediction"
    );
    client.disconnect().await.unwrap();
    wait(&resumed, |v| v.snapshot().status == ViewStatus::Stopped).await;
    let other = harness.client().await;
    let Response::Snapshot { tabs, .. } = other.request(Request::Snapshot).await.unwrap() else {
        panic!("snapshot");
    };
    assert!(matches!(tabs[0].terminal, TerminalState::Running(_)));
    harness.close().await;
}

#[tokio::test]
async fn stop_bypasses_full_local_input_queue_and_never_kills_the_server_shell() {
    let harness = Harness::new();
    let client = harness.client().await;
    let id = tab(&client).await;
    let view = client
        .attach(id, Geometry::new(80, 24).unwrap(), true)
        .await
        .unwrap();
    // Do not yield: the bounded local queue must report the first rejection.
    let mut rejected = false;
    for _ in 0..1000 {
        if view.input(vec![b'x'; 16 * 1024]).is_err() {
            rejected = true;
            break;
        }
    }
    assert!(rejected);
    view.stop();
    view.resume();
    view.pause(); // Stop must be sticky across cloned-handle actions.
    assert!(view.input(b"must not reactivate".to_vec()).is_err());
    wait(&view, |v| {
        matches!(
            v.snapshot().status,
            ViewStatus::Stopped | ViewStatus::Failed(_)
        )
    })
    .await;
    assert!(
        client
            .attach(id, Geometry::new(80, 24).unwrap(), true)
            .await
            .is_err(),
        "a stopped core cannot reinterpret an old ticket as sequence zero"
    );
    let Response::Snapshot { tabs, .. } = client.request(Request::Snapshot).await.unwrap() else {
        panic!("snapshot");
    };
    assert!(matches!(tabs[0].terminal, TerminalState::Running(_)));
    client.disconnect().await.unwrap();
    harness.close().await;
}

#[tokio::test]
async fn final_output_survives_exit_and_explicit_reopen_uses_a_new_sequence_domain() {
    let harness = Harness::new();
    let client = harness.client().await;
    let id = tab(&client).await;
    let view = client
        .attach(id, Geometry::new(80, 24).unwrap(), true)
        .await
        .unwrap();
    wait(&view, |v| v.snapshot().frame.is_some()).await;
    view.input(
        b"printf '\\106\\111\\116\\101\\114\\137\\116\\101\\124\\111\\126\\105\\n'; exit 17\n"
            .to_vec(),
    )
    .unwrap();
    wait(&view, |v| v.snapshot().status == ViewStatus::Ended(17)).await;
    assert!(text(&view).contains("FINAL_NATIVE"));
    let Response::Snapshot { tabs, .. } = client.request(Request::Snapshot).await.unwrap() else {
        panic!("snapshot");
    };
    assert!(matches!(tabs[0].terminal, TerminalState::Ended { .. }));
    assert!(
        client
            .attach(id, Geometry::new(80, 24).unwrap(), true)
            .await
            .is_err()
    );
    client
        .request(Request::ReopenTab { tab: id })
        .await
        .unwrap();
    let replacement = client
        .attach(id, Geometry::new(80, 24).unwrap(), true)
        .await
        .unwrap();
    replacement
        .input(
            b"printf '\\116\\105\\127\\137\\107\\105\\116\\105\\122\\101\\124\\111\\117\\116\\n'\n"
                .to_vec(),
        )
        .unwrap();
    wait(&replacement, |v| {
        text(v).contains("NEW_GENERATION") && v.snapshot().unacked_bytes == 0
    })
    .await;
    assert!(!text(&replacement).contains("FINAL_NATIVE"));
    assert_eq!(view.snapshot().status, ViewStatus::Ended(17));
    client.disconnect().await.unwrap();
    harness.close().await;
}

#[tokio::test]
async fn cached_views_do_not_form_an_arc_cycle_with_the_parent_client() {
    let harness = Harness::new();
    let client = harness.client().await;
    let id = tab(&client).await;
    let view = client
        .attach(id, Geometry::new(80, 24).unwrap(), true)
        .await
        .unwrap();
    let weak = std::sync::Arc::downgrade(&client);
    drop(client);
    assert!(weak.upgrade().is_none());
    view.stop();
    wait(&view, |v| v.snapshot().status == ViewStatus::Stopped).await;
    harness.close().await;
}

#[tokio::test]
async fn native_client_rejects_unpinned_certificate_and_incompatible_bootstrap() {
    let harness = Harness::new();
    let mut bootstrap = bootstrap(harness.directory.clone()).await.unwrap();
    bootstrap.protocol = 99;
    assert!(
        Client::connect("127.0.0.1".into(), bootstrap.clone())
            .await
            .is_err()
    );
    bootstrap.protocol = 1;
    bootstrap.hashes = vec![[42; 32]];
    assert!(
        Client::connect("127.0.0.1".into(), bootstrap)
            .await
            .is_err()
    );
    harness.close().await;
}

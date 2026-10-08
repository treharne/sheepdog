use sheepdog_core::Geometry;
use sheepdog_runtime::{InputError, InputResult, Program, Snapshot, Terminal};
use std::sync::Arc;
use std::time::Duration;

fn spawn(script: &str) -> Terminal {
    Terminal::spawn(Program {
        executable: "/bin/sh".into(),
        args: vec!["-c".into(), script.into()],
        directory: std::env::temp_dir(),
        geometry: Geometry::new(80, 24).unwrap(),
    })
    .unwrap()
}

fn text(snapshot: &Snapshot) -> String {
    let mut text = String::new();
    for row in 0..snapshot.frame.rows() {
        for col in 0..snapshot.frame.cols() {
            text.push_str(snapshot.frame.cell_content(row, col));
        }
        text.push('\n');
    }
    text
}

async fn until(terminal: &Terminal, predicate: impl Fn(&Snapshot) -> bool) -> Arc<Snapshot> {
    let mut receiver = terminal.subscribe();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = receiver.borrow_and_update().clone();
            if predicate(&current) {
                return current;
            }
            assert!(
                current.exit.is_none(),
                "terminal exited early: {:?}; screen={}",
                current.exit,
                text(&current)
            );
            receiver.changed().await.unwrap();
        }
    })
    .await
    .expect("terminal snapshot deadline")
}

async fn cat() -> Terminal {
    let terminal = spawn("stty -echo; printf 'READY\\n'; exec cat");
    until(&terminal, |s| text(s).contains("READY")).await;
    terminal
}

#[tokio::test]
async fn shared_terminal_has_per_attachment_sequences_and_echo_checkpoints() {
    let terminal = cat().await;
    let a = terminal.attach().await.unwrap();
    let b = terminal.attach().await.unwrap();
    assert_eq!(
        terminal.input(a, 1, b"alpha\n".to_vec()).await.unwrap(),
        InputResult::Accepted
    );
    let first = until(&terminal, |s| {
        s.checkpoints[&a.attachment_id()].echo_ack == 1
    })
    .await;
    assert!(text(&first).contains("alpha"));
    assert_eq!(first.checkpoints[&b.attachment_id()].echo_ack, 0);
    assert!(!first.checkpoints[&a.attachment_id()].prediction_allowed);
    assert_eq!(
        terminal.input(b, 1, b"beta\n".to_vec()).await.unwrap(),
        InputResult::Accepted
    );
    let second = until(&terminal, |s| {
        s.checkpoints[&b.attachment_id()].echo_ack == 1
    })
    .await;
    assert!(text(&second).contains("alpha") && text(&second).contains("beta"));
    assert_eq!(second.screen(a).unwrap().echo_ack, 1);
    assert_eq!(second.screen(b).unwrap().echo_ack, 1);
    terminal.close().await.unwrap();
}

#[tokio::test]
async fn reconnect_replay_is_not_written_twice_and_old_connection_cannot_steal_geometry() {
    let terminal = cat().await;
    let a = terminal.attach().await.unwrap();
    terminal
        .input(a, 1, b"UNIQUE_MARKER\n".to_vec())
        .await
        .unwrap();
    until(&terminal, |s| {
        s.checkpoints[&a.attachment_id()].echo_ack == 1
    })
    .await;
    terminal.detach(a).await.unwrap();
    let resumed = terminal.resume(a).await.unwrap();
    assert_eq!(
        terminal
            .input(resumed, 1, b"UNIQUE_MARKER\n".to_vec())
            .await
            .unwrap(),
        InputResult::Replay
    );
    let error = terminal.input(a, 2, b"stale\n".to_vec()).await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<InputError>(),
        Some(&InputError::StaleConnection)
    );
    assert!(
        terminal
            .resize(a, Geometry::new(20, 10).unwrap())
            .await
            .is_err()
    );
    assert!(terminal.snapshot().screen(a).is_none());
    terminal
        .input(resumed, 2, b"AFTER_REPLAY\n".to_vec())
        .await
        .unwrap();
    let snapshot = until(&terminal, |s| {
        s.checkpoints[&resumed.attachment_id()].echo_ack == 2
    })
    .await;
    assert_eq!(text(&snapshot).matches("UNIQUE_MARKER").count(), 1);
    assert!(text(&snapshot).contains("AFTER_REPLAY"));
    terminal.close().await.unwrap();
}

#[tokio::test]
async fn disconnect_all_views_preserves_process_and_reconnect_access() {
    let terminal = cat().await;
    let a = terminal.attach().await.unwrap();
    terminal.detach(a).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(terminal.snapshot().exit.is_none());
    let b = terminal.resume(a).await.unwrap();
    terminal
        .input(b, 1, b"still_alive\n".to_vec())
        .await
        .unwrap();
    until(&terminal, |s| text(s).contains("still_alive")).await;
    terminal.close().await.unwrap();
}

#[tokio::test]
async fn last_interaction_resizes_one_authoritative_terminal() {
    let terminal = cat().await;
    let a = terminal.attach().await.unwrap();
    let b = terminal.attach().await.unwrap();
    let small = Geometry::new(40, 12).unwrap();
    terminal.resize(a, small).await.unwrap();
    assert_eq!(terminal.snapshot().geometry, small);
    assert_eq!(terminal.snapshot().frame.cols(), 40);
    let large = Geometry::new(100, 30).unwrap();
    terminal.resize(b, large).await.unwrap();
    assert_eq!(terminal.snapshot().geometry, large);
    assert!(terminal.snapshot().screen(a).is_some());
    assert!(terminal.snapshot().screen(b).is_some());
    terminal
        .input(a, 1, b"promote-small\n".to_vec())
        .await
        .unwrap();
    assert_eq!(terminal.snapshot().geometry, small);
    terminal
        .input(b, 1, b"promote-large\n".to_vec())
        .await
        .unwrap();
    assert_eq!(terminal.snapshot().geometry, large);
    terminal.focus(a).await.unwrap();
    assert_eq!(terminal.snapshot().geometry, small);
    terminal.close().await.unwrap();
}

#[tokio::test]
async fn exit_preserves_final_output_and_status() {
    let terminal = spawn("printf 'FINAL_OUTPUT\\n'; exit 7");
    let snapshot = until(&terminal, |s| s.exit.is_some()).await;
    assert_eq!(snapshot.exit.as_ref().unwrap().code, Some(7));
    assert!(snapshot.exit.as_ref().unwrap().error.is_none());
    assert!(text(&snapshot).contains("FINAL_OUTPUT"));
}

#[tokio::test]
async fn close_bypasses_backpressured_input() {
    let terminal = spawn("stty -echo -icanon; printf 'READY\\n'; exec sleep 30");
    until(&terminal, |s| text(s).contains("READY")).await;
    let a = terminal.attach().await.unwrap();
    let mut seq = 1;
    for _ in 0..64 {
        match terminal
            .input(a, seq, vec![b'x'; sheepdog_runtime::MAX_INPUT])
            .await
        {
            Ok(_) => seq += 1,
            Err(e) if e.downcast_ref::<InputError>() == Some(&InputError::Busy) => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(seq > 1);
    let exit = tokio::time::timeout(Duration::from_secs(3), terminal.close())
        .await
        .unwrap()
        .unwrap();
    assert!(exit.signal.is_some());
}

#[tokio::test]
async fn full_input_budget_does_not_stop_output_drain() {
    let terminal = spawn(
        "stty -echo -icanon; printf 'READY\\n'; sleep 0.2; printf 'OUTPUT_WHILE_FULL\\n'; exec sleep 30",
    );
    until(&terminal, |s| text(s).contains("READY")).await;
    let a = terminal.attach().await.unwrap();
    let mut seq = 1;
    loop {
        match terminal
            .input(a, seq, vec![b'x'; sheepdog_runtime::MAX_INPUT])
            .await
        {
            Ok(_) => seq += 1,
            Err(e) if e.downcast_ref::<InputError>() == Some(&InputError::Busy) => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    let snapshot = until(&terminal, |s| text(s).contains("OUTPUT_WHILE_FULL")).await;
    assert_eq!(snapshot.checkpoints[&a.attachment_id()].accepted, seq - 1);
    terminal.close().await.unwrap();
}

fn process_running(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let state = stat
        .rsplit_once(')')
        .unwrap()
        .1
        .trim_start()
        .chars()
        .next()
        .unwrap();
    state != 'Z' && state != 'X'
}

#[tokio::test]
async fn closing_a_terminal_stops_descendants_too() {
    let terminal = spawn("sleep 30 & printf 'CHILD:%s\\n' \"$!\"; wait");
    let snapshot = until(&terminal, |s| text(s).contains("CHILD:")).await;
    let screen = text(&snapshot);
    let pid: u32 = screen
        .split("CHILD:")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(process_running(pid));
    terminal.close().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while process_running(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn close_escalates_when_the_process_ignores_term() {
    let terminal = spawn("trap '' TERM; printf 'READY\\n'; while :; do sleep 1; done");
    until(&terminal, |s| text(s).contains("READY")).await;
    let exit = tokio::time::timeout(Duration::from_secs(3), terminal.close())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exit.signal, Some(libc::SIGKILL));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_response_follows_snapshot_publication() {
    let terminal = cat().await;
    for _ in 0..100 {
        let c = terminal.attach().await.unwrap();
        assert!(terminal.snapshot().screen(c).is_some());
        terminal.detach(c).await.unwrap();
        assert!(terminal.snapshot().screen(c).is_none());
        terminal.retire(c).await.unwrap();
    }
    terminal.close().await.unwrap();
}

#[tokio::test]
async fn missing_executable_is_a_launch_error_not_a_running_terminal() {
    let result = Terminal::spawn(Program {
        executable: "/nonexistent/sheepdog-shell".into(),
        args: vec![],
        directory: std::env::temp_dir(),
        geometry: Geometry::new(80, 24).unwrap(),
    });
    assert!(result.is_err());
}

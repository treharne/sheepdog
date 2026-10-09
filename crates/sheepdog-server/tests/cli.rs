use sheepdog_server::protocol::Bootstrap;
use std::process::Stdio;
use std::time::Duration;

#[tokio::test]
async fn server_binary_and_ssh_bootstrap_helper_work_together() {
    let directory = std::env::temp_dir().join(format!("sheepdog-cli-{}", rand::random::<u64>()));
    let mut server = tokio::process::Command::new(env!("CARGO_BIN_EXE_sheepdog-server"))
        .arg("--state-dir")
        .arg(&directory)
        .args(["serve", "--bind", "127.0.0.1:0", "--shell", "/bin/sh"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !directory.join("server.sock").exists() {
            assert!(
                server.try_wait().unwrap().is_none(),
                "server exited before binding"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_sheepdog-server"))
        .arg("--state-dir")
        .arg(&directory)
        .arg("bootstrap")
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bootstrap: Bootstrap = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(bootstrap.protocol, 1);
    assert_ne!(bootstrap.port, 0);
    assert_eq!(bootstrap.hashes.len(), 7);
    assert_ne!(bootstrap.token, [0; 32]);
    unsafe {
        libc::kill(server.id().unwrap() as i32, libc::SIGTERM);
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(5), server.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(!directory.join("server.sock").exists());
    std::fs::remove_dir_all(directory).unwrap();
}

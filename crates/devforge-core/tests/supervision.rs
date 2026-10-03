//! Supervision: exec-provider spawn via PTY, readiness pattern, stop semantics.

use devforge_core::ipc::{Reply, Verb};
use devforge_core::server::Engine;
use serde_json::json;

#[tokio::test]
async fn start_ready_stop_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let dev = tmp.path().join(".devforge");
    std::fs::create_dir_all(&dev).unwrap();
    std::fs::write(
        dev.join("scenario.toml"),
        r#"
[scenario]
name = "t"

[services.sleeper]
provider = "exec"
command = "sh -c 'echo boot; while true; do sleep 1; done'"
ready_when = "boot"

[jobs.nothing]
command = "true"
"#,
    )
    .unwrap();

    let engine = Engine::open(tmp.path().to_path_buf()).await.unwrap();

    // start with wait_for_ready → state up
    let reply = engine
        .dispatch(Verb::ServiceStart {
            name: "sleeper".into(),
            wait_for_ready: Some(true),
        })
        .await;
    let Reply::Ok(state) = reply else {
        panic!("start failed: {reply:?}");
    };
    assert_eq!(state["state"], json!("up"), "{state}");

    // status shows live up state across the IPC surface of truth
    let reply = engine.dispatch(Verb::ScenarioStatus).await;
    let Reply::Ok(status) = reply else {
        panic!("status failed")
    };
    assert_eq!(status["services"][0]["state"], json!("up"), "{status}");

    // stop → state idle; logs recorded
    let reply = engine
        .dispatch(Verb::ServiceStop {
            name: "sleeper".into(),
        })
        .await;
    matches!(reply, Reply::Ok(_));

    let status = engine.dispatch(Verb::ScenarioStatus).await;
    let Reply::Ok(status) = status else {
        panic!("status failed")
    };
    assert_eq!(status["services"][0]["state"], json!("idle"), "{status}");

    let reply = engine
        .dispatch(Verb::ServiceLogs {
            name: "sleeper".into(),
            tail: None,
        })
        .await;
    let Reply::Ok(logs) = reply else {
        panic!("logs failed")
    };
    let lines = logs["lines"].as_array().unwrap();
    assert!(
        lines
            .iter()
            .any(|l| l["line"].as_str().unwrap().contains("boot")),
        "boot line missing in log tail: {lines:?}"
    );
}

#[tokio::test]
async fn job_run_records_result() {
    let tmp = tempfile::tempdir().unwrap();
    let dev = tmp.path().join(".devforge");
    std::fs::create_dir_all(&dev).unwrap();
    std::fs::write(
        dev.join("scenario.toml"),
        r#"
[scenario]
name = "t"

[jobs.echoer]
command = "echo hello-from-job"
"#,
    )
    .unwrap();
    let engine = Engine::open(tmp.path().to_path_buf()).await.unwrap();

    let reply = engine
        .dispatch(Verb::JobRun {
            name: "echoer".into(),
        })
        .await;
    let Reply::Ok(v) = reply else {
        panic!("job failed: {reply:?}")
    };
    assert_eq!(v["exit_code"], serde_json::json!(0));
    assert_eq!(v["success"], serde_json::json!(true));
    assert!(
        v["tail"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap().contains("hello-from-job"))
    );

    // fail case
    std::fs::write(
        dev.join("scenario.toml"),
        r#"
[scenario]
name = "t"

[jobs.failer]
command = "sh -c 'echo oops >&2; exit 3'"
"#,
    )
    .unwrap();
    engine.dispatch(Verb::ScenarioReload).await;
    let reply = engine
        .dispatch(Verb::JobRun {
            name: "failer".into(),
        })
        .await;
    let Reply::Ok(v) = reply else {
        panic!("job failed: {reply:?}")
    };
    assert_eq!(v["exit_code"], serde_json::json!(3));
    assert_eq!(v["success"], serde_json::json!(false));
    assert!(
        v["tail"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap().contains("oops"))
    );

    // unknown job
    let reply = engine
        .dispatch(Verb::JobRun {
            name: "nope".into(),
        })
        .await;
    assert!(matches!(reply, Reply::Err { .. }));
}

//! IPC round-trip: verb in, Reply out, over a real unix socket.

use std::sync::Arc;

use devforge_core::ipc::{Reply, Verb};
use devforge_core::server::Engine;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

async fn connect(
    socket: &std::path::Path,
) -> (
    tokio::io::Lines<tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>,
    tokio::net::unix::OwnedWriteHalf,
) {
    let stream = UnixStream::connect(socket).await.unwrap();
    let (r, w) = stream.into_split();
    (BufReader::new(r).lines(), w)
}

async fn roundtrip(
    reader: &mut tokio::io::Lines<tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>>,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    verb: &Verb,
) -> Reply {
    let mut line = serde_json::to_vec(verb).unwrap();
    line.push(b'\n');
    writer.write_all(&line).await.unwrap();
    let out = reader.next_line().await.unwrap().unwrap();
    serde_json::from_str(&out).unwrap()
}

#[tokio::test]
async fn status_roundtrip_and_unknown_verb() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join(".devforge/socket");
    std::fs::create_dir_all(tmp.path().join(".devforge")).unwrap();

    let engine = Arc::new(Engine::open(tmp.path().to_path_buf()).await.unwrap());
    tokio::spawn(engine.serve());
    // wait for bind
    for _ in 0..50 {
        if UnixStream::connect(&socket).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let (mut reader, mut writer) = connect(&socket).await;

    match roundtrip(&mut reader, &mut writer, &Verb::ScenarioStatus).await {
        Reply::Ok(v) => {
            assert_eq!(v["services"], serde_json::json!([]));
            assert!(v["name"].is_string());
        }
        other => panic!("expected Ok, got {other:?}"),
    }

    match roundtrip(
        &mut reader,
        &mut writer,
        &Verb::ScenarioStart {
            profile: Some("nope".into()),
        },
    )
    .await
    {
        Reply::Err { message } => assert!(message.contains("not defined"), "{message}"),
        other => panic!("expected Err, got {other:?}"),
    }

    match roundtrip(
        &mut reader,
        &mut writer,
        &Verb::ServiceLogs {
            name: "web".into(),
            tail: None,
        },
    )
    .await
    {
        Reply::Err { message } => assert!(message.contains("unknown service"), "{message}"),
        other => panic!("expected Err, got {other:?}"),
    }
}

#[tokio::test]
async fn second_bind_rejected_while_daemon_live() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = Arc::new(Engine::open(tmp.path().to_path_buf()).await.unwrap());
    tokio::spawn(engine.clone().serve());
    let socket = engine.socket_path().await;
    for _ in 0..50 {
        if UnixStream::connect(&socket).await.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let second = Arc::new(Engine::open(tmp.path().to_path_buf()).await.unwrap());
    let err = second.serve().await.unwrap_err();
    assert!(err.to_string().contains("already answers"), "{err}");
}

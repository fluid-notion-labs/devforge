//! `devforge` daemon: engine + IPC socket. Prints the socket path on boot.

use std::sync::Arc;

use devforge_core::server::Engine;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "devforge_core=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let root = std::env::current_dir()?;
    let engine = Arc::new(Engine::open(root).await?);

    let socket = engine.socket_path().await;
    println!("devforge: IPC socket at {}", socket.display());

    engine.serve().await?;
    Ok(())
}

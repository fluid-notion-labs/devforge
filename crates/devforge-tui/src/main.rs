use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        // Socket resolution stays trivial for now: env override or the
        // well-known path (plan/spec.md).
        let socket = std::env::var("DEVFORGE_SOCKET")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(devforge_core::ipc::DEFAULT_SOCKET_PATH));
        devforge_tui::run(&socket).await
    })?;
    Ok(())
}

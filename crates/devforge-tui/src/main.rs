fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel();
        devforge_tui::run(devforge_core::ipc::DEFAULT_SOCKET_PATH, rx).await
    })?;
    Ok(())
}

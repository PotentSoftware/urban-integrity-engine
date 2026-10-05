/// SIGUSR1 signal handler for triggering a manual graph checkpoint.
///
/// Only compiled on Unix targets. Send `SIGUSR1` to the process to trigger
/// an immediate CSR snapshot + checkpoint write.
///
/// # Example
/// ```sh
/// kill -USR1 $(pgrep urban-integrity)
/// ```
#[cfg(unix)]
pub async fn sigusr1_checkpoint_task(checkpoint_tx: tokio::sync::mpsc::Sender<()>) {
    use tokio::signal::unix::{signal, SignalKind};
    use tracing::{info, warn};

    let mut stream = match signal(SignalKind::user_defined1()) {
        Ok(s) => s,
        Err(e) => {
            warn!(
                "Failed to register SIGUSR1 handler: {} — manual checkpoints disabled",
                e
            );
            return;
        }
    };

    info!("SIGUSR1 checkpoint handler registered");
    loop {
        stream.recv().await;
        info!("SIGUSR1 received — triggering manual checkpoint");
        if checkpoint_tx.send(()).await.is_err() {
            info!("Checkpoint channel closed, SIGUSR1 handler exiting");
            break;
        }
    }
}

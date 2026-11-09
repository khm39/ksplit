use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

use ksplit::CancelToken;

/// First Ctrl+C asks the running operation to stop at a record boundary; the second exits at once.
pub fn install(cancel: CancelToken, writes_checkpoint: bool) {
    let count = Arc::new(AtomicU8::new(0));
    let result = ctrlc::set_handler(move || {
        if count.fetch_add(1, Ordering::SeqCst) == 0 {
            cancel.cancel();
            if writes_checkpoint {
                eprintln!(
                    "\ninterrupted: writing a checkpoint; press Ctrl+C again to abort immediately"
                );
            } else {
                eprintln!("\ninterrupted");
            }
        } else {
            std::process::exit(5);
        }
    });
    if let Err(e) = result {
        eprintln!("warning: could not install the Ctrl+C handler: {e}");
    }
}

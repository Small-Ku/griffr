use std::path::PathBuf;

const CHECKPOINT_ENV: &str = "GRIFFR_TEST_CHECKPOINT";
const READY_PATH_ENV: &str = "GRIFFR_TEST_CHECKPOINT_READY";

/// Stop a test child process at a filesystem checkpoint until its parent kills
/// it. This deliberately avoids unwinding so process-kill tests observe the
/// same on-disk state as an abrupt launcher termination.
pub(crate) fn hit(name: &str) {
    if std::env::var(CHECKPOINT_ENV).ok().as_deref() != Some(name) {
        return;
    }

    let ready_path = std::env::var_os(READY_PATH_ENV)
        .map(PathBuf::from)
        .expect("process-kill checkpoint requires a ready path");
    std::fs::write(&ready_path, name).expect("write process-kill checkpoint signal");

    loop {
        std::thread::park();
    }
}

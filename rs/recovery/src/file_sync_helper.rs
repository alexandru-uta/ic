use crate::{
    cli::wait_for_confirmation,
    command_helper::exec_cmd,
    error::{RecoveryError, RecoveryResult},
    ssh_helper,
};
use core::time;
use ic_http_utils::file_downloader::FileDownloader;
use ic_types::ReplicaVersion;
use slog::{info, warn, Logger};
use std::{
    fs::{self, File, ReadDir},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    thread,
};

/// Given the name and replica version of a binary, download the artifact to the
/// target directory, unzip it, and add executable permissions.
/// Returns a [PathBuf] to the downloaded binary.
pub async fn download_binary(
    logger: &Logger,
    replica_version: ReplicaVersion,
    binary_name: String,
    target_dir: PathBuf,
) -> RecoveryResult<PathBuf> {
    let binary_url = format!(
        "https://download.dfinity.systems/ic/{}/release/{}.gz",
        replica_version, binary_name
    );

    let mut file = target_dir.join(format!("{}.gz", binary_name));

    info!(logger, "Downloading {} to {:?}...", binary_name, file);
    let file_downloader = FileDownloader::new(None);
    file_downloader
        .download_file(&binary_url, &file, None)
        .await
        .map_err(|e| RecoveryError::download_error(binary_url, &file, e))?;

    info!(logger, "Unzipping file...");
    let mut gunzip = Command::new("gunzip");
    gunzip.arg(file);

    if let Some(out) = exec_cmd(&mut gunzip)? {
        info!(logger, "{}", out);
    }

    file = target_dir.join(binary_name);

    info!(logger, "Adding permissions...");
    let mut chmod = Command::new("chmod");
    chmod.arg("+x").arg(file.clone());

    if let Some(out) = exec_cmd(&mut chmod)? {
        info!(logger, "{}", out);
    }

    Ok(file)
}

pub fn rsync_with_retries(
    logger: &Logger,
    excludes: Vec<&str>,
    src: &str,
    target: &str,
    require_confirmation: bool,
    key_file: Option<&PathBuf>,
    retries: usize,
) -> RecoveryResult<Option<String>> {
    for _ in 0..retries {
        match rsync(
            logger,
            excludes.clone(),
            src,
            target,
            require_confirmation,
            key_file,
        ) {
            Err(e) => {
                warn!(logger, "Rsync failed: {:?}, retrying...", e);
            }
            success => return success,
        }
        thread::sleep(time::Duration::from_secs(10));
    }
    Err(RecoveryError::UnexpectedError("All retries failed".into()))
}

/// Copy the files from src to target using [rsync](https://linux.die.net/man/1/rsync) and options `--delete`, `-acP`.
/// File and directory names part of the `excludes` vector are discarded.
pub fn rsync(
    logger: &Logger,
    excludes: Vec<&str>,
    src: &str,
    target: &str,
    require_confirmation: bool,
    key_file: Option<&PathBuf>,
) -> RecoveryResult<Option<String>> {
    let mut rsync = Command::new("rsync");
    rsync.arg("--delete").arg("-acP").arg("--no-g");
    excludes
        .iter()
        .map(|e| format!("--exclude={}", e))
        .for_each(|e| {
            rsync.arg(e);
        });
    rsync.arg(src).arg(target);
    rsync.arg("-e").arg(ssh_helper::get_rsync_ssh_arg(key_file));
    info!(logger, "");
    info!(logger, "About to execute:");
    info!(logger, "{:?}", rsync);
    if require_confirmation {
        wait_for_confirmation(logger);
    }
    info!(logger, "Starting transfer, waiting for output...");
    match exec_cmd(&mut rsync) {
        Err(RecoveryError::CommandError(Some(24), msg)) => {
            warn!(logger, "Masking rsync warning (code 24)");
            info!(logger, "{}", msg);
            Ok(Some(msg))
        }
        Ok(Some(msg)) => {
            info!(logger, "{}", msg);
            Ok(Some(msg))
        }
        res => res,
    }
}

pub fn write_file(file: &Path, content: String) -> RecoveryResult<()> {
    let mut f = File::create(file).map_err(|e| RecoveryError::file_error(file, e))?;
    write!(f, "{}", content).map_err(|e| RecoveryError::file_error(file, e))?;
    Ok(())
}

pub fn write_bytes(file: &Path, bytes: Vec<u8>) -> RecoveryResult<()> {
    fs::write(file, bytes).map_err(|e| RecoveryError::file_error(file, e))
}

pub fn read_file(file: &Path) -> RecoveryResult<String> {
    fs::read_to_string(file).map_err(|e| RecoveryError::file_error(file, e))
}

pub fn create_dir(path: &Path) -> RecoveryResult<()> {
    fs::create_dir_all(path).map_err(|e| RecoveryError::dir_error(path, e))
}

pub fn read_dir(path: &Path) -> RecoveryResult<ReadDir> {
    fs::read_dir(path).map_err(|e| RecoveryError::dir_error(path, e))
}

pub fn path_exists(path: &Path) -> RecoveryResult<bool> {
    path.try_exists()
        .map_err(|e| RecoveryError::IoError(String::from("Cannot check if the path exists"), e))
}

pub fn remove_dir(path: &Path) -> RecoveryResult<()> {
    if path_exists(path)? {
        fs::remove_dir_all(path).map_err(|e| RecoveryError::dir_error(path, e))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn path_exists_should_return_true() {
        let tmp = tempdir().expect("Couldn't create a temp test directory");

        assert!(path_exists(tmp.path()).unwrap());
    }

    #[test]
    fn path_exists_should_return_false() {
        let tmp = tempdir().expect("Couldn't create a temp test directory");
        let non_existing_path = tmp.path().join("non_existing_subdir");

        assert!(!path_exists(&non_existing_path).unwrap());
    }
}

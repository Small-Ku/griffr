use std::io::ErrorKind;
use std::path::Path;

use crate::error::{Error, Result};
use crate::{ArtifactDigest, ArtifactExpectation, ArtifactProof, ArtifactSource};

use super::extract::{
    move_path_replace, move_path_replace_cross_volume, move_path_replace_cross_volume_observed,
    MovePathReplaceOutcome,
};
use super::reuse::make_temp_write_path;

fn digest_error(
    expectation: &ArtifactExpectation,
    digest: &ArtifactDigest,
    context: &'static str,
) -> Error {
    Error::Message {
        context,
        detail: format!(
            "Artifact {} failed verification: expected size/hash {:?}/{}, got {}/{}",
            expectation.logical_path(),
            expectation.expected_size(),
            expectation.expected_hash(),
            digest.bytes,
            digest.hash
        ),
    }
}

pub(crate) fn verify_artifact(
    path: &Path,
    expectation: &ArtifactExpectation,
    source: ArtifactSource,
) -> Result<ArtifactProof> {
    if let Some(issue) = crate::task_pool::verify::build_issue(
        path,
        expectation.logical_path(),
        expectation.expected_hash(),
        expectation.expected_size(),
    ) {
        return Err(Error::Message {
            context: "Integrity error: ",
            detail: format!(
                "Artifact {} failed verification: {:?}",
                expectation.logical_path(),
                issue.kind
            ),
        });
    }
    ArtifactProof::from_verified_path(path, expectation.clone(), source).map_err(|source| {
        Error::IoAt {
            action: "query file metadata/stat for",
            path: path.to_path_buf(),
            source,
        }
    })
}

fn proof_from_digest(
    path: &Path,
    expectation: &ArtifactExpectation,
    source: ArtifactSource,
    digest: &ArtifactDigest,
) -> Result<ArtifactProof> {
    if !expectation.accepts_digest(digest) {
        return Err(digest_error(expectation, digest, "Integrity error: "));
    }
    let proof =
        ArtifactProof::from_verified_path(path, expectation.clone(), source).map_err(|source| {
            Error::IoAt {
                action: "query file metadata/stat for",
                path: path.to_path_buf(),
                source,
            }
        })?;
    if proof.observed_size() != digest.bytes {
        return Err(Error::Message {
            context: "Integrity error: ",
            detail: format!(
                "Committed artifact {} has size {}, expected written size {}",
                path.display(),
                proof.observed_size(),
                digest.bytes
            ),
        });
    }
    Ok(proof)
}

pub(crate) fn commit_unchecked_artifact(source_path: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::IoAt {
            action: "create directory",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    move_path_replace_cross_volume(source_path, destination)
}

pub(crate) fn commit_verified_artifact(
    source_path: &Path,
    destination: &Path,
    expectation: &ArtifactExpectation,
    source: ArtifactSource,
) -> Result<ArtifactProof> {
    let verified = verify_artifact(source_path, expectation, source)?;
    let digest = ArtifactDigest::new(
        verified.observed_size(),
        expectation.expected_hash().clone(),
    );
    match move_path_replace_cross_volume_observed(source_path, destination, &digest)? {
        MovePathReplaceOutcome::Renamed => verify_artifact(destination, expectation, source),
        MovePathReplaceOutcome::Copied => {
            proof_from_digest(destination, expectation, source, &digest)
        }
    }
}

pub(crate) fn commit_observed_artifact(
    source_path: &Path,
    destination: &Path,
    expectation: &ArtifactExpectation,
    source: ArtifactSource,
    digest: &ArtifactDigest,
) -> Result<ArtifactProof> {
    if !expectation.accepts_digest(digest) {
        return Err(digest_error(expectation, digest, "Integrity error: "));
    }
    #[cfg(test)]
    crate::test_checkpoint::hit("artifact.before_replace");
    let _ = move_path_replace_cross_volume_observed(source_path, destination, digest)?;
    #[cfg(test)]
    crate::test_checkpoint::hit("artifact.after_replace");
    proof_from_digest(destination, expectation, source, digest)
}

pub(crate) fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::IoAt {
            action: "create directory",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let temp = make_temp_write_path(path)?;
    let result = (|| -> Result<()> {
        use std::io::Write;
        let mut output = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(|source| Error::IoAt {
                action: "write to file",
                path: temp.clone(),
                source,
            })?;
        output.write_all(bytes).map_err(|source| Error::IoAt {
            action: "write to file",
            path: temp.clone(),
            source,
        })?;
        output.sync_all().map_err(|source| Error::IoAt {
            action: "write to file",
            path: temp.clone(),
            source,
        })?;
        drop(output);
        #[cfg(test)]
        crate::test_checkpoint::hit("atomic_write.after_sync");
        move_path_replace(&temp, path)
    })();
    if result.is_err() {
        match std::fs::remove_file(&temp) {
            Ok(()) => {}
            Err(source) if source.kind() == ErrorKind::NotFound => {}
            Err(_) => {}
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use md5::{Digest, Md5};

    use super::*;

    #[test]
    fn observed_commit_replaces_destination_and_returns_proof() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.tmp");
        let destination = temp.path().join("file.bin");
        let payload = b"new-data";
        std::fs::write(&source, payload).unwrap();
        std::fs::write(&destination, b"old-data").unwrap();
        let md5 = griffr_core::to_hex(&Md5::digest(payload));
        let expectation =
            ArtifactExpectation::md5("file.bin", &md5, Some(payload.len() as u64)).unwrap();
        let digest =
            ArtifactDigest::new(payload.len() as u64, crate::ContentHash::md5(&md5).unwrap());

        let proof = commit_observed_artifact(
            &source,
            &destination,
            &expectation,
            ArtifactSource::Download,
            &digest,
        )
        .unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), payload);
        assert!(!source.exists());
        assert_eq!(proof.source(), ArtifactSource::Download);
        assert!(proof.is_current());
    }

    #[test]
    fn observed_commit_rejects_bad_digest_before_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.tmp");
        let destination = temp.path().join("file.bin");
        std::fs::write(&source, b"new-data").unwrap();
        std::fs::write(&destination, b"old-data").unwrap();
        let expectation =
            ArtifactExpectation::md5("file.bin", "00000000000000000000000000000000", Some(8))
                .unwrap();
        let digest = ArtifactDigest::new(
            8,
            crate::ContentHash::md5(griffr_core::to_hex(&Md5::digest(b"new-data"))).unwrap(),
        );

        let error = commit_observed_artifact(
            &source,
            &destination,
            &expectation,
            ArtifactSource::Archive,
            &digest,
        )
        .unwrap_err();

        assert!(error.to_string().contains("failed verification"));
        assert_eq!(std::fs::read(&destination).unwrap(), b"old-data");
        assert!(source.exists());
    }
}

mod artifact;
mod delete_manifest;
pub(crate) mod extract;
mod patch_manifest;
pub(crate) mod path_safety;
mod reuse;

pub(crate) use artifact::{
    commit_observed_artifact, commit_unchecked_artifact, commit_verified_artifact, verify_artifact,
    write_atomic_bytes,
};
pub(crate) use delete_manifest::{apply_delete_files_manifest_async, parse_delete_files_manifest};
pub(crate) use extract::{
    commit_deferred_paths, commit_file_job, create_extract_staging_dir, CommitFileJob,
};
pub(crate) use patch_manifest::{
    apply_extracted_vfs_patch_manifest, apply_patch_deletes, apply_patch_entry, clean_patch_apply,
    commit_deferred_patch_files, prepare_patch_apply, release_patch_base, resume_patch_apply,
};
pub(crate) use reuse::{
    classify_reuse_mode, commit_partial_download, copy_verified_file_async, create_hardlink,
    hash_file_prefix_into_hasher, make_partial_download_path, make_temp_write_path,
    storage_volume_group_key, storage_volume_id, ReuseMode,
};

#[cfg(test)]
pub(crate) use reuse::write_file;

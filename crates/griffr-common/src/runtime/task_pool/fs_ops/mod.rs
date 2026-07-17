mod delete_manifest;
pub(crate) mod extract;
mod patch_manifest;
pub(crate) mod path_safety;
mod reuse;

pub(crate) use delete_manifest::{apply_delete_files_manifest, parse_delete_files_manifest};
pub(crate) use extract::{commit_staged_extract, make_extract_staging_dir};
pub(crate) use patch_manifest::{
    apply_extracted_vfs_patch_manifest, execute_patch_transaction, resume_patch_transaction,
};
pub(crate) use reuse::{
    classify_reuse_mode, commit_partial_download, create_hardlink, dispatch_io,
    hash_file_prefix_into_hasher, make_partial_download_path, reuse_verified_file,
    storage_volume_group_key, storage_volume_id, ReuseMethod, ReuseMode,
};

#[cfg(test)]
pub(crate) use reuse::{make_temp_write_path, write_file};

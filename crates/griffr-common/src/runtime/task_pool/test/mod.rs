use super::*;
use crate::runtime::task_pool::fs_ops::{
    make_partial_download_path, make_temp_write_path, write_file,
};
use md5::{Digest, Md5};
use std::io::Write;
use std::path::PathBuf;
use tempfile::tempdir;
use zip::write::FileOptions;

mod archive;
mod download;

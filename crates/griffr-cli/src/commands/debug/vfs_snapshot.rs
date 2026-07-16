use std::path::PathBuf;

use anyhow::{Context, Result};
use griffr_common::api::crypto;
use griffr_common::runtime::{list_files_with_extension, GAME_FILES_NAME};

use crate::GlobalOptions;
use griffr_common::runtime::{decrypt_config_ini, detect_local_install, resolve_named_path};

pub async fn detect(path: PathBuf, _opts: GlobalOptions) -> Result<()> {
    let local = detect_local_install(&path).await?;
    println!("install_path={}", local.install_path.display());
    println!("config_ini={}", local.config_ini.path.display());
    println!("known_game={:?}", local.game_id);
    println!("known_region={:?}", local.region_id);
    println!("known_channel={:?}", local.channel_id);
    for (key, value) in &local.config_ini.fields {
        println!("{}={}", key, value);
    }
    Ok(())
}

pub async fn config_ini(path: PathBuf, _opts: GlobalOptions) -> Result<()> {
    let ini = decrypt_config_ini(&path).await?;
    print!("{}", ini.raw);
    Ok(())
}

pub async fn game_files(path: PathBuf, _opts: GlobalOptions) -> Result<()> {
    let game_files_path = resolve_named_path(&path, GAME_FILES_NAME);
    let encrypted = compio::fs::read(&game_files_path)
        .await
        .with_context(|| format!("Failed to read {}", game_files_path.display()))?;
    let decrypted = crypto::decrypt_game_files(&encrypted)
        .with_context(|| format!("Failed to decrypt {}", game_files_path.display()))?;
    print!("{}", decrypted);
    Ok(())
}

pub async fn res_index(path: PathBuf, key: Option<String>, _opts: GlobalOptions) -> Result<()> {
    let key = key.unwrap_or_else(|| crypto::RES_INDEX_KEY.to_string());
    let mut targets = Vec::new();

    if path.is_dir() {
        targets = list_files_with_extension(path.clone(), "json").await?;
        if targets.is_empty() {
            anyhow::bail!("No .json files found under {}", path.display());
        }
    } else {
        targets.push(path);
    }

    let multi_file = targets.len() > 1;
    for target in targets {
        let encrypted_b64 = compio::fs::read(&target)
            .await
            .with_context(|| format!("Failed to read {}", target.display()))
            .and_then(|bytes| {
                String::from_utf8(bytes)
                    .with_context(|| format!("{} is not valid UTF-8 text", target.display()))
            })?;
        let decrypted = crypto::decrypt_res_index(encrypted_b64.trim(), &key)
            .with_context(|| format!("Failed to decrypt {}", target.display()))?;
        if multi_file {
            println!("=== {} ===", target.display());
        }
        print!("{}", decrypted);
        if !decrypted.ends_with('\n') {
            println!();
        }
    }

    Ok(())
}

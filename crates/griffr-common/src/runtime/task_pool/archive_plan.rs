use std::path::Path;

use rapidhash::RapidHashMap;

use crate::api::types::PackFile;
use crate::error::{Error, Result};

use super::types::ArchivePart;

#[derive(Debug, Clone)]
pub struct ArchiveGroup {
    pub base_name: String,
    pub parts: Vec<ArchivePart>,
}

pub fn plan_archive_groups(packs: &[PackFile], archive_dir: &Path) -> Result<Vec<ArchiveGroup>> {
    if packs.is_empty() {
        return Err(Error::Config("No archives to process".to_string()));
    }

    let mut grouped: RapidHashMap<String, Vec<ArchivePart>> = RapidHashMap::default();
    for pack in packs {
        let filename = pack
            .filename()
            .ok_or_else(|| Error::Config("Failed to extract archive filename".to_string()))?
            .to_string();
        let base_name = pack
            .archive_base_name()
            .ok_or_else(|| {
                Error::Config("Pack URL did not end with .zip or a numeric .zip.<part>".to_string())
            })?
            .to_string();
        let sequence = pack.archive_sequence().ok_or_else(|| {
            Error::Config("Pack URL did not contain a valid archive sequence".to_string())
        })?;

        grouped.entry(base_name).or_default().push(ArchivePart {
            sequence,
            url: pack.url.clone(),
            dest: archive_dir.join(&filename),
            logical_path: filename,
            expected_md5: pack.md5.clone(),
            expected_size: pack.size(),
        });
    }

    let mut groups = grouped
        .into_iter()
        .map(|(base_name, mut parts)| {
            parts.sort_by(|left, right| {
                left.sequence
                    .cmp(&right.sequence)
                    .then_with(|| left.logical_path.cmp(&right.logical_path))
            });
            ArchiveGroup { base_name, parts }
        })
        .collect::<Vec<_>>();
    groups.sort_by(|left, right| left.base_name.cmp(&right.base_name));
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(url: &str) -> PackFile {
        PackFile {
            url: url.to_string(),
            md5: "md5".to_string(),
            package_size: "1".to_string(),
        }
    }

    #[test]
    fn groups_and_orders_archive_parts_by_numeric_sequence() {
        let groups = plan_archive_groups(
            &[
                pack("https://cdn.example/game.zip.12"),
                pack("https://cdn.example/other.zip"),
                pack("https://cdn.example/game.zip.2"),
            ],
            Path::new("downloads"),
        )
        .unwrap();

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].base_name, "game");
        assert_eq!(
            groups[0]
                .parts
                .iter()
                .map(|part| part.sequence)
                .collect::<Vec<_>>(),
            vec![2, 12]
        );
        assert_eq!(groups[1].base_name, "other");
        assert_eq!(groups[1].parts[0].sequence, 0);
    }
}

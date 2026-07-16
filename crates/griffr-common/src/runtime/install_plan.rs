use crate::api::types::PackageInfo;

/// Estimate peak bytes required to stage a full install package.
///
/// Launcher metadata is not always consistent about whether `total_size`
/// describes the extracted payload or the archive set, so planning uses the
/// larger of the declared total and the sum of archive parts.
pub fn required_install_bytes(package: &PackageInfo) -> u64 {
    let archive_bytes = package.packs.iter().map(|part| part.size()).sum::<u64>();
    let declared_total = package.total_size.parse::<u64>().unwrap_or(0);
    archive_bytes.max(declared_total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::PackFile;

    fn package(total_size: &str, part_sizes: &[u64]) -> PackageInfo {
        PackageInfo {
            packs: part_sizes
                .iter()
                .enumerate()
                .map(|(index, size)| PackFile {
                    url: format!("https://example.com/full.zip.{:03}", index + 1),
                    md5: format!("part-{index}"),
                    package_size: size.to_string(),
                })
                .collect(),
            total_size: total_size.to_string(),
            file_path: "https://example.com/files".to_string(),
            game_files_md5: None,
        }
    }

    #[test]
    fn uses_larger_declared_total() {
        assert_eq!(required_install_bytes(&package("20", &[8, 7])), 20);
    }

    #[test]
    fn falls_back_to_archive_sum_when_declared_total_is_invalid() {
        assert_eq!(required_install_bytes(&package("invalid", &[4, 6])), 10);
    }
}

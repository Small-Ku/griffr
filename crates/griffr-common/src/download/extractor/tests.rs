use super::*;
use std::io::Write;

#[test]
fn test_multi_volume_extractor() -> Result<()> {
    let temp_dir = tempfile::tempdir()?;
    let base_path = temp_dir.path();

    // 1. Create a zip archive and split it
    let zip_path = base_path.join("test.zip");
    let file = std::fs::File::create(&zip_path)?;
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file("hello.txt", zip::write::FileOptions::<()>::default())?;
    zip.write_all(b"Hello, World!")?;
    zip.finish()?;

    let data = std::fs::read(&zip_path)?;
    let chunk_size = 5;
    let mut volumes = Vec::new();
    for (i, chunk) in data.chunks(chunk_size).enumerate() {
        let volume_path = base_path.join(format!("test.zip.{:03}", i + 1));
        std::fs::write(&volume_path, chunk)?;
        volumes.push(volume_path);
    }

    // 2. Extract
    let extractor = MultiVolumeExtractor::new(volumes)?;
    let inspection = extractor.inspect_patch_payload(None)?;
    let output_dir = base_path.join("output");
    std::fs::create_dir(&output_dir)?;
    extractor.extract_to_with_progress(
        &output_dir,
        None,
        &inspection,
        2,
        64,
        None::<fn(u64, u64)>,
    )?;

    // 3. Verify
    let output_file = output_dir.join("hello.txt");
    assert!(output_file.exists());
    let content = std::fs::read_to_string(output_file)?;
    assert_eq!(content, "Hello, World!");

    Ok(())
}

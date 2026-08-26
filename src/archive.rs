use flate2::read::GzDecoder;
use std::fs;
use std::io;
use std::path::Path;
use tar::Archive as TarArchive;
use zip::read::ZipArchive;

/// Returns true if the path is a `.tar.gz` or `.tgz` file.
fn is_tar_gz(path: &Path) -> bool {
    let name = path.file_name().and_then(|e| e.to_str()).unwrap_or("");
    name.ends_with(".tar.gz") || name.ends_with(".tgz")
}

/// Returns true if the path looks like a supported archive file.
///
/// A bare `.gz` (not `.tar.gz`/`.tgz`) is a single-file gzip stream, which
/// `extract_archive` doesn't know how to decompress on its own — so it's
/// deliberately not counted as "archive" here, even though the extension
/// alone might suggest it.
pub fn is_archive(path: &Path) -> bool {
    if is_tar_gz(path) {
        return true;
    }
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("zip") | Some("tar")
    )
}

/// Extracts the given archive into `dest_dir`.
/// Supports `.zip`, `.tar`, `.tar.gz`, and `.tgz`.
pub fn extract_archive(archive: &Path, dest_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dest_dir)?;
    if is_tar_gz(archive) {
        extract_tar_gz(archive, dest_dir)
    } else {
        match archive.extension().and_then(|e| e.to_str()) {
            Some("zip") => extract_zip(archive, dest_dir),
            Some("tar") => extract_tar(archive, dest_dir),
            Some("gz") => extract_tar_gz(archive, dest_dir),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("unsupported archive type: {}", archive.display()),
            )),
        }
    }
}

fn extract_zip(archive: &Path, dest_dir: &Path) -> io::Result<()> {
    let file = fs::File::open(archive)?;
    let mut zip = ZipArchive::new(file).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to open zip: {e}"),
        )
    })?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("failed to read zip entry: {e}"),
            )
        })?;
        let out_path = dest_dir.join(entry.mangled_name());
        if entry.is_dir() {
            fs::create_dir_all(&out_path)?;
        } else {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out_file = fs::File::create(&out_path)?;
            io::copy(&mut entry, &mut out_file)?;
        }
    }
    Ok(())
}

fn extract_tar(archive: &Path, dest_dir: &Path) -> io::Result<()> {
    let file = fs::File::open(archive)?;
    let mut tar = TarArchive::new(file);
    tar.unpack(dest_dir).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to extract tar: {e}"),
        )
    })
}

fn extract_tar_gz(archive: &Path, dest_dir: &Path) -> io::Result<()> {
    let file = fs::File::open(archive)?;
    let decoder = GzDecoder::new(file);
    let mut tar = TarArchive::new(decoder);
    tar.unpack(dest_dir).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to extract tar.gz: {e}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn is_archive_detects_supported_extensions() {
        assert!(is_archive(Path::new("archive.zip")));
        assert!(is_archive(Path::new("archive.tar")));
        assert!(is_archive(Path::new("archive.tar.gz")));
        assert!(is_archive(Path::new("archive.tgz")));
        assert!(!is_archive(Path::new("file.txt")));
        // A bare .gz (not .tar.gz/.tgz) is a single-file gzip stream that
        // extract_archive can't decompress on its own — must not be offered
        // as extractable.
        assert!(!is_archive(Path::new("file.gz")));
    }

    #[test]
    fn extract_zip_creates_files() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("test.zip");
        let extract_dir = dir.path().join("extracted");

        // Create a minimal zip file in memory
        {
            let zip_file = fs::File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(zip_file);
            zip.start_file("hello.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(b"hello world").unwrap();
            zip.finish().unwrap();
        }

        extract_archive(&zip_path, &extract_dir).unwrap();
        assert!(extract_dir.join("hello.txt").exists());
        assert_eq!(
            fs::read(extract_dir.join("hello.txt")).unwrap(),
            b"hello world"
        );
    }
}

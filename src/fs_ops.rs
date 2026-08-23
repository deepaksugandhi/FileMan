use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Which clipboard operation a set of clipboard paths belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardOp {
    Copy,
    Cut,
}

fn copy_file(src: &Path, dest: &Path) -> io::Result<()> {
    if dest.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("destination already exists: {}", dest.display()),
        ));
    }
    fs::copy(src, dest)?;
    Ok(())
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let entry_dest = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &entry_dest)?;
        } else {
            copy_file(&entry.path(), &entry_dest)?;
        }
    }
    Ok(())
}

fn check_dest_dir(dest_dir: &Path) -> io::Result<()> {
    if !dest_dir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("destination folder not found: {}", dest_dir.display()),
        ));
    }
    Ok(())
}

fn src_name(src: &Path) -> io::Result<std::ffi::OsString> {
    src.file_name()
        .map(|n| n.to_os_string())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source has no file name"))
}

/// Copies `src` (file or directory) into `dest_dir`, keeping its file name.
/// Fails if an item with the same name already exists in `dest_dir`.
pub fn copy_item(src: &Path, dest_dir: &Path) -> io::Result<PathBuf> {
    let dest = dest_dir.join(src_name(src)?);
    check_dest_dir(dest_dir)?;
    if src.is_dir() {
        copy_dir_recursive(src, &dest)?;
    } else {
        copy_file(src, &dest)?;
    }
    Ok(dest)
}

/// Moves `src` (file or directory) into `dest_dir`.
/// Uses a cheap rename when possible; falls back to copy + delete when the
/// source and destination are on different volumes.
pub fn move_item(src: &Path, dest_dir: &Path) -> io::Result<()> {
    let dest = dest_dir.join(src_name(src)?);
    check_dest_dir(dest_dir)?;
    if dest.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("destination already exists: {}", dest.display()),
        ));
    }
    match fs::rename(src, &dest) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Cross-volume move: copy then delete the source.
            if src.is_dir() {
                copy_dir_recursive(src, &dest)?;
                fs::remove_dir_all(src)
            } else {
                fs::copy(src, &dest)?;
                fs::remove_file(src)
            }
        }
    }
}

fn validate_name(name: &str) -> io::Result<&str> {
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." || name.contains('\\') || name.contains('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid name: {name:?}"),
        ));
    }
    Ok(name)
}

/// Copies `src` (file or directory) directly to `dest` (an exact path, not a
/// parent directory). Creates intermediate directories as needed. Will NOT
/// overwrite an existing `dest`.
pub fn copy_item_to(src: &Path, dest: &Path) -> io::Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    if src.is_dir() {
        copy_dir_recursive(src, dest)?;
    } else {
        copy_file(src, dest)?;
    }
    Ok(())
}

fn new_path_in(parent: &Path, raw_name: &str) -> io::Result<PathBuf> {
    let name = validate_name(raw_name)?;
    let path = parent.join(name);
    if path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("an item named {name:?} already exists"),
        ));
    }
    Ok(path)
}

/// Renames `src` to `new_name` in place. Rejects empty names, path
/// separators, and names that collide with existing siblings.
pub fn rename_item(src: &Path, new_name: &str) -> io::Result<()> {
    let parent = src.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "source has no parent folder")
    })?;
    let dest = new_path_in(parent, new_name)?;
    fs::rename(src, &dest)
}

/// Creates an empty folder `parent/name`. Fails if it already exists.
pub fn create_folder(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let dir = new_path_in(parent, name)?;
    fs::create_dir(&dir)?;
    Ok(dir)
}

/// Creates an empty file `parent/name`. Fails if one already exists.
pub fn create_file(parent: &Path, name: &str) -> io::Result<PathBuf> {
    let file = new_path_in(parent, name)?;
    fs::File::create_new(&file)?;
    Ok(file)
}

/// Sends every path to the Recycle Bin (never permanently deletes).
pub fn delete_to_trash(paths: &[PathBuf]) -> Result<(), trash::Error> {
    for path in paths {
        trash::delete(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dest_dir = dir.path().join("out");
        fs::create_dir(&dest_dir).unwrap();
        fs::write(&src, b"hello").unwrap();

        let copied = copy_item(&src, &dest_dir).unwrap();

        assert_eq!(copied, dest_dir.join("src"));
        assert_eq!(fs::read(copied).unwrap(), b"hello");
    }

    #[test]
    fn copies_a_directory_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("tree");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("top.txt"), b"1").unwrap();
        fs::write(src.join("sub").join("deep.txt"), b"2").unwrap();
        let dest_dir = dir.path().join("out");
        fs::create_dir(&dest_dir).unwrap();

        copy_item(&src, &dest_dir).unwrap();

        assert_eq!(
            fs::read(dest_dir.join("tree").join("top.txt")).unwrap(),
            b"1"
        );
        assert_eq!(
            fs::read(dest_dir.join("tree").join("sub").join("deep.txt")).unwrap(),
            b"2"
        );
    }

    #[test]
    fn copy_refuses_to_overwrite_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.txt");
        fs::write(&src, b"new").unwrap();
        let dest = dir.path().join("a.txt");
        fs::write(&dest, b"old").unwrap();

        let err = copy_item(&src, dir.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&dest).unwrap(), b"old");
    }

    #[test]
    fn move_refuses_missing_destination_folder() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.txt");
        fs::write(&src, b"x").unwrap();

        let err = move_item(&src, &dir.path().join("missing")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn moves_a_file_within_the_same_volume() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        let src = dir.path().join("a.txt");
        fs::write(&src, b"data").unwrap();

        move_item(&src, &sub).unwrap();

        assert!(!src.exists());
        assert_eq!(fs::read(sub.join("a.txt")).unwrap(), b"data");
    }

    #[test]
    fn moves_a_directory_and_keeps_contents() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("folder");
        fs::create_dir_all(src.join("inner")).unwrap();
        fs::write(src.join("inner").join("f.txt"), b"x").unwrap();
        let sub = dir.path().join("elsewhere");
        fs::create_dir(&sub).unwrap();

        move_item(&src, &sub).unwrap();

        assert!(!src.exists());
        assert_eq!(
            fs::read(sub.join("folder").join("inner").join("f.txt")).unwrap(),
            b"x"
        );
    }

    #[test]
    fn move_refuses_to_overwrite_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.txt");
        fs::write(&src, b"new").unwrap();
        let dest = dir.path().join("a.txt");
        fs::write(&dest, b"old").unwrap();

        let err = move_item(&src, dir.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&dest).unwrap(), b"old");
    }

    #[test]
    fn rename_updates_the_name_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("before.txt");
        fs::write(&src, b"z").unwrap();

        rename_item(&src, "after.txt").unwrap();

        assert!(!src.exists());
        assert_eq!(fs::read(dir.path().join("after.txt")).unwrap(), b"z");
    }

    #[test]
    fn rename_rejects_bad_names_and_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.txt");
        fs::write(&src, b"z").unwrap();
        fs::write(dir.path().join("b.txt"), b"y").unwrap();

        assert_eq!(
            rename_item(&src, "").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            rename_item(&src, "x\\y").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            rename_item(&src, "..").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            rename_item(&src, "b.txt").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        // The source must be untouched by all rejected attempts.
        assert!(src.exists());
    }

    #[test]
    fn creates_new_folder_and_file() {
        let dir = tempfile::tempdir().unwrap();

        create_folder(dir.path(), "New Folder").unwrap();
        assert!(dir.path().join("New Folder").is_dir());

        create_file(dir.path(), "note.txt").unwrap();
        assert!(dir.path().join("note.txt").is_file());
        assert_eq!(fs::read(dir.path().join("note.txt")).unwrap(), b"");
    }

    #[test]
    fn create_refuses_collisions_and_bad_names() {
        let dir = tempfile::tempdir().unwrap();
        create_folder(dir.path(), "dup").unwrap();

        assert_eq!(
            create_folder(dir.path(), "dup").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            create_file(dir.path(), "a/b").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            create_file(dir.path(), "").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}

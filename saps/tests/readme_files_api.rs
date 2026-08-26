//! Compile-checks the calls shown in the README's `## Files` section.
#![cfg(feature = "files")]

use saps::errors::file::FileError;
use saps::errors::saps::SapsError;
use saps::files::adapters::file_paths::{file_path, file_path_str, folder_path, folder_path_str};
use saps::files::api::files::{delete, exists, read, write};
use saps::files::api::folders::{all_child_files, child_folders, create_folder};
use saps::files::api::mods::extract_path;
use saps::files::engines::async_io::mem::AsyncMemIo;
use saps::files::engines::blocking_io::disk::BlockingDiskIo;
use saps::files::engines::blocking_io::mem::BlockingMemIo;
use saps::files::files::mem_files::guard::MemFileGuard;
use saps::files::files::mem_files::streamer::MemFileStreamer;
use saps::files::paths::{FilePath, FolderPath, Path};
use saps::kernel::transaction::{CursorIndex, DeleteSlice, InsertSlice, Operation, Transaction};

#[allow(dead_code)]
fn paths_section() -> Result<(), FileError> {
    let file = Path::<FilePath>::build("/srv/project".to_string(), "src/main.rs".to_string())?;
    let _whole = Path::<FilePath>::new("/srv/project/src/main.rs")?;
    let _tuple: Path<FilePath> =
        ("/srv/project".to_string(), "src/main.rs".to_string()).try_into()?;

    let folder: Path<FolderPath> = file.clone().into_parent_folder()?;
    let _sibling: Path<FilePath> = folder.clone().into_child_file("lib.rs")?;
    let _nested: Path<FolderPath> = folder.into_child_folder("models")?;

    let _rel: &std::path::Path = file.relative();
    let _rel_s: String = file.relative_string();
    let _root: String = file.root_string();
    let _ext: Option<String> = file.extension();
    let mut file = file;
    file.set_extension("bak");

    let _ = file_path_str("/srv/project", "src/main.rs")?;
    let _ = folder_path_str("/srv/project", "src")?;
    let _ = file_path(std::path::Path::new("/srv/project"), "src/main.rs")?;
    let _ = folder_path(std::path::Path::new("/srv/project"), "src")?;
    Ok(())
}

#[allow(dead_code)]
fn file_operations() -> Result<(), FileError> {
    let path = file_path_str("/srv/project", "src/main.rs")?;
    write::blocking(&BlockingDiskIo, &path, "fn main() {}")?;
    let _contents: String = read::blocking(&BlockingDiskIo, &path)?;
    let _there: bool = exists::blocking(&BlockingDiskIo, &path);
    delete::blocking(&BlockingDiskIo, &path)?;
    Ok(())
}

#[allow(dead_code)]
async fn async_file_operations() -> Result<(), FileError> {
    let path = file_path_str("/srv/project", "src/main.rs")?;
    let store = AsyncMemIo::new();
    write::asynchronous(&store, &path, "fn main() {}").await?;
    let _contents = read::asynchronous(&store, &path).await?;
    Ok(())
}

#[allow(dead_code)]
fn folder_operations() -> Result<(), FileError> {
    let dir = folder_path_str("/srv/project", "src")?;
    create_folder::blocking(&BlockingDiskIo, dir.clone())?;
    let _immediate: Vec<Path<FolderPath>> = child_folders::blocking(&BlockingDiskIo, dir.clone())?;
    let _everything: Vec<Path<FilePath>> = all_child_files::blocking(&BlockingDiskIo, dir)?;
    let _resolved = extract_path::blocking(&BlockingDiskIo, "/srv/project", "models/user")?;
    Ok(())
}

#[allow(dead_code)]
fn buffers_and_transactions() -> Result<(), FileError> {
    let path = file_path_str("/srv/project", "src/main.rs")?;
    let store = BlockingMemIo::new();
    let mut guard = MemFileGuard::new(&store);

    guard.add_file(&path)?;
    let _file = guard.get_file(&path)?;
    guard.snapshot()?;

    let tx = Transaction::new(vec![
        Operation::Insert(InsertSlice {
            position: CursorIndex { line: 0, col: 0 },
            content: "// header\n".to_string(),
        }),
        Operation::Delete(DeleteSlice { position: CursorIndex { line: 4, col: 0 }, length: 12 }),
    ]);
    let _update: Vec<u8> = guard.apply_transaction(&path, &tx)?;

    let streamer = MemFileStreamer::new(&guard);
    let _n = streamer.count();
    let _empty = streamer.is_empty();
    for (_relative_path, _state) in streamer.states() {}
    Ok(())
}

#[allow(dead_code)]
fn error_conversion(path: Path<FilePath>) -> Result<String, SapsError> {
    Ok(read::blocking(&BlockingDiskIo, &path)?)
}

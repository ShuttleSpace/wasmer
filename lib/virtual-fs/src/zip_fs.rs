use futures::future::BoxFuture;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use zip::result::ZipError;

use crate::{
    DirEntry, FileOpener, FileSystem, FileType, FsError, Metadata, OpenOptions, OpenOptionsConfig,
    ReadDir, StaticFile,
};

/// A read-only filesystem backed by a ZIP archive.
///
/// The filesystem builds a path index from ZIP central directory entries during
/// initialization, and only decompresses file contents on `open`.
#[derive(Debug, Clone)]
pub struct ZipArchiveFileSystem {
    archive_bytes: Arc<[u8]>,
    nodes: BTreeMap<PathBuf, ZipNode>,
    children: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
}

#[derive(Debug, Clone)]
enum ZipNode {
    Dir(Metadata),
    File(ZipFileEntry),
}

#[derive(Debug, Clone)]
struct ZipFileEntry {
    index: usize,
    metadata: Metadata,
}

impl ZipArchiveFileSystem {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, FsError> {
        Self::from_bytes(bytes.into())
    }

    fn from_bytes(bytes: Vec<u8>) -> Result<Self, FsError> {
        let archive_bytes: Arc<[u8]> = Arc::from(bytes);
        let mut nodes = BTreeMap::new();
        let mut children = BTreeMap::new();

        nodes.insert(PathBuf::from("/"), ZipNode::Dir(dir_metadata()));

        let mut archive = zip::ZipArchive::new(Cursor::new(archive_bytes.as_ref()))
            .map_err(map_zip_error)?;

        for index in 0..archive.len() {
            let file = archive.by_index(index).map_err(map_zip_error)?;
            if file.name().is_empty() {
                continue;
            }

            let normalized = normalize_path(Path::new(file.name()), true)?;
            if normalized == Path::new("/") {
                continue;
            }

            if file.is_dir() {
                ensure_dir(&mut nodes, &mut children, &normalized)?;
                continue;
            }

            let parent = parent_path(&normalized);
            ensure_dir(&mut nodes, &mut children, &parent)?;

            if matches!(nodes.get(&normalized), Some(ZipNode::Dir(_))) {
                return Err(FsError::InvalidData);
            }

            nodes.insert(
                normalized.clone(),
                ZipNode::File(ZipFileEntry {
                    index,
                    metadata: file_metadata(file.size()),
                }),
            );
            children.entry(parent).or_default().insert(normalized);
        }

        Ok(Self {
            archive_bytes,
            nodes,
            children,
        })
    }

    fn node_for_path(&self, path: &Path) -> Result<(&PathBuf, &ZipNode), FsError> {
        let normalized = normalize_path(path, false)?;
        self.nodes
            .get_key_value(&normalized)
            .ok_or(FsError::EntryNotFound)
    }
}

impl FileSystem for ZipArchiveFileSystem {
    fn readlink(&self, _path: &Path) -> Result<PathBuf, FsError> {
        Err(FsError::InvalidInput)
    }

    fn read_dir(&self, path: &Path) -> Result<ReadDir, FsError> {
        let (normalized, node) = self.node_for_path(path)?;
        if !matches!(node, ZipNode::Dir(_)) {
            return Err(FsError::BaseNotDirectory);
        }

        let entries = self
            .children
            .get(normalized)
            .into_iter()
            .flatten()
            .map(|child| DirEntry {
                path: child.clone(),
                metadata: self.metadata(child),
            })
            .collect();

        Ok(ReadDir::new(entries))
    }

    fn create_dir(&self, path: &Path) -> Result<(), FsError> {
        if self.metadata(path).is_ok() {
            return Err(FsError::AlreadyExists);
        }

        let parent = parent_path(&normalize_path(path, false)?);
        match self.metadata(&parent) {
            Ok(parent_meta) if parent_meta.is_dir() => Err(FsError::PermissionDenied),
            Ok(_) => Err(FsError::BaseNotDirectory),
            Err(err) => Err(err),
        }
    }

    fn remove_dir(&self, path: &Path) -> Result<(), FsError> {
        let meta = self.metadata(path)?;
        if !meta.is_dir() {
            return Err(FsError::BaseNotDirectory);
        }
        Err(FsError::PermissionDenied)
    }

    fn rename<'a>(&'a self, from: &'a Path, to: &'a Path) -> BoxFuture<'a, Result<(), FsError>> {
        Box::pin(async move {
            let _ = self.metadata(from)?;
            let to_parent = parent_path(&normalize_path(to, false)?);
            let parent_meta = self.metadata(&to_parent)?;
            if !parent_meta.is_dir() {
                return Err(FsError::BaseNotDirectory);
            }
            Err(FsError::PermissionDenied)
        })
    }

    fn metadata(&self, path: &Path) -> Result<Metadata, FsError> {
        let (_, node) = self.node_for_path(path)?;
        match node {
            ZipNode::Dir(meta) => Ok(meta.clone()),
            ZipNode::File(file) => Ok(file.metadata.clone()),
        }
    }

    fn symlink_metadata(&self, path: &Path) -> Result<Metadata, FsError> {
        self.metadata(path)
    }

    fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        let meta = self.metadata(path)?;
        if !meta.is_file() {
            return Err(FsError::NotAFile);
        }
        Err(FsError::PermissionDenied)
    }

    fn new_open_options(&self) -> OpenOptions<'_> {
        OpenOptions::new(self)
    }

    fn mount(
        &self,
        _name: String,
        _path: &Path,
        _fs: Box<dyn FileSystem + Send + Sync>,
    ) -> Result<(), FsError> {
        Err(FsError::Unsupported)
    }
}

impl FileOpener for ZipArchiveFileSystem {
    fn open(
        &self,
        path: &Path,
        conf: &OpenOptionsConfig,
    ) -> Result<Box<dyn crate::VirtualFile + Send + Sync + 'static>, FsError> {
        let normalized = normalize_path(path, false)?;
        let parent = parent_path(&normalized);

        if !self.metadata(&parent)?.is_dir() {
            return Err(FsError::BaseNotDirectory);
        }

        if conf.would_mutate() {
            return Err(FsError::PermissionDenied);
        }

        let entry = match self.nodes.get(&normalized) {
            Some(ZipNode::File(file)) => file,
            Some(ZipNode::Dir(_)) => return Err(FsError::NotAFile),
            None if conf.create() || conf.create_new() => return Err(FsError::PermissionDenied),
            None => return Err(FsError::EntryNotFound),
        };

        let mut archive = zip::ZipArchive::new(Cursor::new(self.archive_bytes.as_ref()))
            .map_err(map_zip_error)?;
        let mut zip_file = archive.by_index(entry.index).map_err(map_zip_error)?;

        let mut bytes = Vec::with_capacity(entry.metadata.len.min(usize::MAX as u64) as usize);
        zip_file.read_to_end(&mut bytes).map_err(FsError::from)?;

        Ok(Box::new(StaticFile::new(bytes)))
    }
}

fn map_zip_error(error: ZipError) -> FsError {
    match error {
        ZipError::FileNotFound => FsError::EntryNotFound,
        ZipError::Io(err) => FsError::from(err),
        _ => FsError::InvalidData,
    }
}

fn normalize_path(path: &Path, allow_empty: bool) -> Result<PathBuf, FsError> {
    if path.as_os_str().is_empty() {
        return if allow_empty {
            Ok(PathBuf::from("/"))
        } else {
            Err(FsError::InvalidInput)
        };
    }

    let mut components = Vec::<OsString>::new();

    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                let _ = components.pop();
            }
            Component::Normal(segment) => components.push(segment.to_os_string()),
        }
    }

    let mut normalized = PathBuf::from("/");
    for segment in components {
        normalized.push(segment);
    }

    Ok(normalized)
}

fn parent_path(path: &Path) -> PathBuf {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn ensure_dir(
    nodes: &mut BTreeMap<PathBuf, ZipNode>,
    children: &mut BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    path: &Path,
) -> Result<(), FsError> {
    let normalized = normalize_path(path, true)?;
    if normalized == Path::new("/") {
        nodes
            .entry(normalized)
            .or_insert_with(|| ZipNode::Dir(dir_metadata()));
        return Ok(());
    }

    if let Some(ZipNode::File(_)) = nodes.get(&normalized) {
        return Err(FsError::InvalidData);
    }

    if nodes.contains_key(&normalized) {
        return Ok(());
    }

    let parent = parent_path(&normalized);
    ensure_dir(nodes, children, &parent)?;

    nodes.insert(normalized.clone(), ZipNode::Dir(dir_metadata()));
    children.entry(parent).or_default().insert(normalized);

    Ok(())
}

fn dir_metadata() -> Metadata {
    Metadata {
        ft: FileType::new_dir(),
        ..Default::default()
    }
}

fn file_metadata(len: u64) -> Metadata {
    Metadata {
        ft: FileType::new_file(),
        len,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use tokio::io::AsyncReadExt;

    fn sample_zip_bytes() -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        writer.add_directory("lib/", options).unwrap();
        writer.add_directory("lib/python/", options).unwrap();

        writer.start_file("lib/python/__init__.py", options).unwrap();
        writer.write_all(b"x = 1\n").unwrap();

        // no explicit directory entries for site-packages/pkg, ensure we still
        // expose implicit directories.
        writer.start_file("site-packages/pkg/mod.py", options).unwrap();
        writer.write_all(b"print('ok')\n").unwrap();

        writer.finish().unwrap().into_inner()
    }

    fn read_dir_paths(fs: &ZipArchiveFileSystem, path: &str) -> Vec<String> {
        let mut entries: Vec<_> = fs
            .read_dir(Path::new(path))
            .unwrap()
            .map(|entry| entry.unwrap().path.display().to_string())
            .collect();
        entries.sort();
        entries
    }

    #[test]
    fn indexes_explicit_and_implicit_directories() {
        let fs = ZipArchiveFileSystem::new(sample_zip_bytes()).unwrap();

        assert!(fs.metadata(Path::new("/")).unwrap().is_dir());
        assert!(fs.metadata(Path::new("/lib/python")).unwrap().is_dir());
        assert!(fs
            .metadata(Path::new("/site-packages/pkg"))
            .unwrap()
            .is_dir());

        assert_eq!(read_dir_paths(&fs, "/"), vec!["/lib", "/site-packages"]);
        assert_eq!(
            read_dir_paths(&fs, "/site-packages/pkg"),
            vec!["/site-packages/pkg/mod.py"]
        );
    }

    #[tokio::test]
    async fn opens_and_reads_files_on_demand() {
        let fs = ZipArchiveFileSystem::new(sample_zip_bytes()).unwrap();

        let mut file = fs
            .new_open_options()
            .read(true)
            .open("/lib/python/__init__.py")
            .unwrap();

        let mut content = String::new();
        file.read_to_string(&mut content).await.unwrap();
        assert_eq!(content, "x = 1\n");
    }

    #[test]
    fn rejects_mutating_open_flags() {
        let fs = ZipArchiveFileSystem::new(sample_zip_bytes()).unwrap();

        let err = fs
            .new_open_options()
            .write(true)
            .open("/lib/python/__init__.py")
            .unwrap_err();
        assert_eq!(err, FsError::PermissionDenied);
    }
}

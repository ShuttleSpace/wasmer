use futures::future::BoxFuture;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use crate::{
    DirEntry, FileOpener, FileSystem, FileType, FsError, Metadata, OpenOptions, OpenOptionsConfig,
    ReadDir, StaticFile, ZipArchiveFileSystem,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
    TarXz,
    SevenZ,
    Rar,
}

impl ArchiveFormat {
    pub fn from_path(path: &Path) -> Option<Self> {
        let file_name = path.file_name()?.to_string_lossy().to_ascii_lowercase();

        if file_name.ends_with(".tar.gz") || file_name.ends_with(".tgz") {
            return Some(Self::TarGz);
        }
        if file_name.ends_with(".tar.xz") || file_name.ends_with(".txz") {
            return Some(Self::TarXz);
        }
        if file_name.ends_with(".7z") {
            return Some(Self::SevenZ);
        }
        if file_name.ends_with(".rar") {
            return Some(Self::Rar);
        }
        if file_name.ends_with(".tar") {
            return Some(Self::Tar);
        }
        if file_name.ends_with(".zip")
            || file_name.ends_with(".jar")
            || file_name.ends_with(".whl")
            || file_name.ends_with(".apk")
        {
            return Some(Self::Zip);
        }

        None
    }
}

#[derive(Debug, Clone)]
pub struct ArchiveFileSystem {
    inner: ArchiveBackend,
}

#[derive(Debug, Clone)]
enum ArchiveBackend {
    Zip(ZipArchiveFileSystem),
    Indexed(IndexedArchiveFileSystem),
}

impl ArchiveFileSystem {
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, FsError> {
        let path = path.as_ref();
        let format = ArchiveFormat::from_path(path).ok_or(FsError::InvalidInput)?;

        match format {
            ArchiveFormat::Zip => {
                let bytes = std::fs::read(path)?;
                let fs = ZipArchiveFileSystem::new(bytes)?;
                Ok(Self {
                    inner: ArchiveBackend::Zip(fs),
                })
            }
            ArchiveFormat::Tar => {
                let bytes = std::fs::read(path)?;
                let fs = IndexedArchiveFileSystem::from_tar_reader(Cursor::new(bytes))?;
                Ok(Self {
                    inner: ArchiveBackend::Indexed(fs),
                })
            }
            ArchiveFormat::TarGz => {
                let bytes = std::fs::read(path)?;
                let decoder = flate2::read::GzDecoder::new(Cursor::new(bytes));
                let fs = IndexedArchiveFileSystem::from_tar_reader(decoder)?;
                Ok(Self {
                    inner: ArchiveBackend::Indexed(fs),
                })
            }
            ArchiveFormat::TarXz => {
                let bytes = std::fs::read(path)?;
                let mut output = Vec::new();
                let mut input = std::io::BufReader::new(Cursor::new(bytes));
                lzma_rs::xz_decompress(&mut input, &mut output)
                    .map_err(|_| FsError::InvalidData)?;
                let fs = IndexedArchiveFileSystem::from_tar_reader(Cursor::new(output))?;
                Ok(Self {
                    inner: ArchiveBackend::Indexed(fs),
                })
            }
            ArchiveFormat::SevenZ => {
                #[cfg(target_arch = "wasm32")]
                {
                    return Err(FsError::Unsupported);
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                let bytes = std::fs::read(path)?;
                let fs = IndexedArchiveFileSystem::from_7z_bytes(bytes)?;
                Ok(Self {
                    inner: ArchiveBackend::Indexed(fs),
                })
                }
            }
            ArchiveFormat::Rar => {
                #[cfg(any(target_arch = "wasm32", target_vendor = "apple"))]
                {
                    return Err(FsError::Unsupported);
                }
                #[cfg(all(not(target_arch = "wasm32"), not(target_vendor = "apple")))]
                {
                let fs = IndexedArchiveFileSystem::from_rar_path(path)?;
                Ok(Self {
                    inner: ArchiveBackend::Indexed(fs),
                })
                }
            }
        }
    }
}

impl FileSystem for ArchiveFileSystem {
    fn readlink(&self, path: &Path) -> Result<PathBuf, FsError> {
        match &self.inner {
            ArchiveBackend::Zip(fs) => fs.readlink(path),
            ArchiveBackend::Indexed(fs) => fs.readlink(path),
        }
    }

    fn read_dir(&self, path: &Path) -> Result<ReadDir, FsError> {
        match &self.inner {
            ArchiveBackend::Zip(fs) => fs.read_dir(path),
            ArchiveBackend::Indexed(fs) => fs.read_dir(path),
        }
    }

    fn create_dir(&self, path: &Path) -> Result<(), FsError> {
        match &self.inner {
            ArchiveBackend::Zip(fs) => fs.create_dir(path),
            ArchiveBackend::Indexed(fs) => fs.create_dir(path),
        }
    }

    fn remove_dir(&self, path: &Path) -> Result<(), FsError> {
        match &self.inner {
            ArchiveBackend::Zip(fs) => fs.remove_dir(path),
            ArchiveBackend::Indexed(fs) => fs.remove_dir(path),
        }
    }

    fn rename<'a>(&'a self, from: &'a Path, to: &'a Path) -> BoxFuture<'a, Result<(), FsError>> {
        match &self.inner {
            ArchiveBackend::Zip(fs) => fs.rename(from, to),
            ArchiveBackend::Indexed(fs) => fs.rename(from, to),
        }
    }

    fn metadata(&self, path: &Path) -> Result<Metadata, FsError> {
        match &self.inner {
            ArchiveBackend::Zip(fs) => fs.metadata(path),
            ArchiveBackend::Indexed(fs) => fs.metadata(path),
        }
    }

    fn symlink_metadata(&self, path: &Path) -> Result<Metadata, FsError> {
        match &self.inner {
            ArchiveBackend::Zip(fs) => fs.symlink_metadata(path),
            ArchiveBackend::Indexed(fs) => fs.symlink_metadata(path),
        }
    }

    fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        match &self.inner {
            ArchiveBackend::Zip(fs) => fs.remove_file(path),
            ArchiveBackend::Indexed(fs) => fs.remove_file(path),
        }
    }

    fn new_open_options(&self) -> OpenOptions<'_> {
        OpenOptions::new(self)
    }

    fn mount(
        &self,
        name: String,
        path: &Path,
        fs: Box<dyn FileSystem + Send + Sync>,
    ) -> Result<(), FsError> {
        match &self.inner {
            ArchiveBackend::Zip(inner) => inner.mount(name, path, fs),
            ArchiveBackend::Indexed(inner) => inner.mount(name, path, fs),
        }
    }
}

impl FileOpener for ArchiveFileSystem {
    fn open(
        &self,
        path: &Path,
        conf: &OpenOptionsConfig,
    ) -> Result<Box<dyn crate::VirtualFile + Send + Sync + 'static>, FsError> {
        match &self.inner {
            ArchiveBackend::Zip(fs) => fs.open(path, conf),
            ArchiveBackend::Indexed(fs) => fs.open(path, conf),
        }
    }
}

#[derive(Debug, Clone)]
struct IndexedArchiveFileSystem {
    nodes: BTreeMap<PathBuf, IndexedNode>,
    children: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
}

#[derive(Debug, Clone)]
enum IndexedNode {
    Dir(Metadata),
    File(IndexedFileEntry),
}

#[derive(Debug, Clone)]
struct IndexedFileEntry {
    bytes: Arc<[u8]>,
    metadata: Metadata,
}

impl IndexedArchiveFileSystem {
    fn empty() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(PathBuf::from("/"), IndexedNode::Dir(dir_metadata()));

        Self {
            nodes,
            children: BTreeMap::new(),
        }
    }

    fn from_tar_reader<R: Read>(reader: R) -> Result<Self, FsError> {
        let mut fs = Self::empty();
        let mut archive = tar::Archive::new(reader);

        for entry in archive.entries()? {
            let mut entry = entry?;
            let normalized = normalize_archive_entry_path(&entry.path()?)?;

            if normalized == Path::new("/") {
                continue;
            }

            let entry_type = entry.header().entry_type();
            if entry_type.is_dir() {
                fs.add_dir(&normalized)?;
                continue;
            }

            if !entry_type.is_file() {
                continue;
            }

            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            fs.add_file(&normalized, bytes)?;
        }

        Ok(fs)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn from_7z_bytes(bytes: Vec<u8>) -> Result<Self, FsError> {
        let mut fs = Self::empty();
        let mut reader =
            sevenz_rust2::ArchiveReader::new(Cursor::new(bytes), sevenz_rust2::Password::empty())
                .map_err(map_7z_error)?;

        reader
            .for_each_entries(|entry, content| {
                let normalized =
                    normalize_archive_entry_name(entry.name()).map_err(std::io::Error::from)?;

                if normalized == Path::new("/") {
                    return Ok(true);
                }

                if entry.is_directory() {
                    fs.add_dir(&normalized).map_err(std::io::Error::from)?;
                    return Ok(true);
                }

                let mut bytes = Vec::new();
                content.read_to_end(&mut bytes)?;
                fs.add_file(&normalized, bytes)
                    .map_err(std::io::Error::from)?;

                Ok(true)
            })
            .map_err(map_7z_error)?;

        Ok(fs)
    }

    #[cfg(all(not(target_arch = "wasm32"), not(target_vendor = "apple")))]
    fn from_rar_path(path: &Path) -> Result<Self, FsError> {
        let mut fs = Self::empty();

        let mut archive = unrar::Archive::new(path)
            .open_for_processing()
            .map_err(map_unrar_error)?;

        loop {
            let next = archive.read_header().map_err(map_unrar_error)?;
            let archive_with_entry = match next {
                Some(value) => value,
                None => break,
            };

            let entry_path = archive_with_entry.entry().filename.clone();
            let is_dir = archive_with_entry.entry().is_directory();
            let normalized = normalize_archive_entry_path(&entry_path)?;

            if normalized == Path::new("/") {
                archive = archive_with_entry.skip().map_err(map_unrar_error)?;
                continue;
            }

            if is_dir {
                fs.add_dir(&normalized)?;
                archive = archive_with_entry.skip().map_err(map_unrar_error)?;
                continue;
            }

            let (bytes, next_archive) = archive_with_entry.read().map_err(map_unrar_error)?;
            fs.add_file(&normalized, bytes)?;
            archive = next_archive;
        }

        Ok(fs)
    }

    fn add_dir(&mut self, path: &Path) -> Result<(), FsError> {
        ensure_dir(&mut self.nodes, &mut self.children, path)
    }

    fn add_file(&mut self, path: &Path, bytes: Vec<u8>) -> Result<(), FsError> {
        let normalized = normalize_path(path, false)?;
        if normalized == Path::new("/") {
            return Err(FsError::InvalidData);
        }

        let parent = parent_path(&normalized);
        ensure_dir(&mut self.nodes, &mut self.children, &parent)?;

        if matches!(self.nodes.get(&normalized), Some(IndexedNode::Dir(_))) {
            return Err(FsError::InvalidData);
        }

        self.nodes.insert(
            normalized.clone(),
            IndexedNode::File(IndexedFileEntry {
                metadata: file_metadata(bytes.len() as u64),
                bytes: Arc::from(bytes),
            }),
        );
        self.children.entry(parent).or_default().insert(normalized);

        Ok(())
    }

    fn node_for_path(&self, path: &Path) -> Result<(&PathBuf, &IndexedNode), FsError> {
        let normalized = normalize_path(path, false)?;
        self.nodes
            .get_key_value(&normalized)
            .ok_or(FsError::EntryNotFound)
    }
}

impl FileSystem for IndexedArchiveFileSystem {
    fn readlink(&self, _path: &Path) -> Result<PathBuf, FsError> {
        Err(FsError::InvalidInput)
    }

    fn read_dir(&self, path: &Path) -> Result<ReadDir, FsError> {
        let (normalized, node) = self.node_for_path(path)?;
        if !matches!(node, IndexedNode::Dir(_)) {
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
            IndexedNode::Dir(meta) => Ok(meta.clone()),
            IndexedNode::File(file) => Ok(file.metadata.clone()),
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

impl FileOpener for IndexedArchiveFileSystem {
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
            Some(IndexedNode::File(file)) => file,
            Some(IndexedNode::Dir(_)) => return Err(FsError::NotAFile),
            None if conf.create() || conf.create_new() => return Err(FsError::PermissionDenied),
            None => return Err(FsError::EntryNotFound),
        };

        Ok(Box::new(StaticFile::new(entry.bytes.to_vec())))
    }
}

fn normalize_archive_entry_name(name: &str) -> Result<PathBuf, FsError> {
    let unix_like = name.replace('\\', "/");
    normalize_path(Path::new(&unix_like), true)
}

fn normalize_archive_entry_path(path: &Path) -> Result<PathBuf, FsError> {
    let unix_like = path.to_string_lossy().replace('\\', "/");
    normalize_path(Path::new(&unix_like), true)
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
    nodes: &mut BTreeMap<PathBuf, IndexedNode>,
    children: &mut BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    path: &Path,
) -> Result<(), FsError> {
    let normalized = normalize_path(path, true)?;
    if normalized == Path::new("/") {
        nodes
            .entry(normalized)
            .or_insert_with(|| IndexedNode::Dir(dir_metadata()));
        return Ok(());
    }

    if let Some(IndexedNode::File(_)) = nodes.get(&normalized) {
        return Err(FsError::InvalidData);
    }

    if nodes.contains_key(&normalized) {
        return Ok(());
    }

    let parent = parent_path(&normalized);
    ensure_dir(nodes, children, &parent)?;

    nodes.insert(normalized.clone(), IndexedNode::Dir(dir_metadata()));
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

#[cfg(not(target_arch = "wasm32"))]
fn map_7z_error(error: sevenz_rust2::Error) -> FsError {
    match error {
        sevenz_rust2::Error::FileNotFound => FsError::EntryNotFound,
        sevenz_rust2::Error::Io(err, _) => FsError::from(err),
        _ => FsError::InvalidData,
    }
}

#[cfg(all(not(target_arch = "wasm32"), not(target_vendor = "apple")))]
fn map_unrar_error(error: unrar::error::UnrarError) -> FsError {
    use unrar::error::Code;

    match error.code {
        Code::EOpen => FsError::EntryNotFound,
        Code::NoMemory => FsError::StorageFull,
        Code::ERead | Code::EWrite | Code::EClose | Code::ECreate => FsError::IOError,
        _ => FsError::InvalidData,
    }
}

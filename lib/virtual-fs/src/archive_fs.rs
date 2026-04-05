use futures::future::BoxFuture;
use std::{
    collections::{BTreeMap, BTreeSet, HashSet, VecDeque},
    ffi::OsString,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};
use shared_buffer::OwnedBuffer;

use crate::{
    DirEntry, FileOpener, FileSystem, FileType, FsError, Metadata, OpenOptions, OpenOptionsConfig,
    ReadDir, StaticFile,
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
    negative_lookup_cache: Arc<Mutex<NegativeLookupCache>>,
}

#[derive(Debug, Clone)]
enum IndexedNode {
    Dir(Metadata),
    File(IndexedFileEntry),
}

#[derive(Debug, Clone)]
struct IndexedFileEntry {
    bytes: OwnedBuffer,
    metadata: Metadata,
}

const DEFAULT_NEGATIVE_LOOKUP_ENTRIES: usize = 32 * 1024;

#[derive(Debug, Default)]
struct NegativeLookupCache {
    max_entries: usize,
    entries: HashSet<PathBuf>,
    order: VecDeque<PathBuf>,
}

impl NegativeLookupCache {
    fn new(max_entries: usize) -> Self {
        Self {
            max_entries,
            ..Default::default()
        }
    }

    fn contains(&self, path: &Path) -> bool {
        self.entries.contains(path)
    }

    fn insert(&mut self, path: PathBuf) {
        if self.entries.insert(path.clone()) {
            self.order.push_back(path);
        }
        while self.entries.len() > self.max_entries {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            let _ = self.entries.remove(&oldest);
        }
    }
}

impl IndexedArchiveFileSystem {
    fn empty() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(PathBuf::from("/"), IndexedNode::Dir(dir_metadata()));

        Self {
            nodes,
            children: BTreeMap::new(),
            negative_lookup_cache: Arc::new(Mutex::new(NegativeLookupCache::new(
                DEFAULT_NEGATIVE_LOOKUP_ENTRIES,
            ))),
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
                bytes: OwnedBuffer::from(bytes),
            }),
        );
        self.children.entry(parent).or_default().insert(normalized);

        Ok(())
    }

    fn node_for_path(&self, path: &Path) -> Result<(&PathBuf, &IndexedNode), FsError> {
        let normalized = normalize_path(path, false)?;
        {
            let cache = self.negative_lookup_cache.lock().map_err(|_| FsError::Lock)?;
            if cache.contains(&normalized) {
                return Err(FsError::EntryNotFound);
            }
        }
        self.nodes
            .get_key_value(&normalized)
            .ok_or_else(|| {
                if let Ok(mut cache) = self.negative_lookup_cache.lock() {
                    cache.insert(normalized);
                }
                FsError::EntryNotFound
            })
    }

    #[cfg(test)]
    fn negative_cache_size(&self) -> usize {
        self.negative_lookup_cache.lock().unwrap().entries.len()
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

        if conf.create() || conf.create_new() || conf.append() || conf.truncate() {
            return Err(FsError::PermissionDenied);
        }

        let entry = match self.nodes.get(&normalized) {
            Some(IndexedNode::File(file)) => file,
            Some(IndexedNode::Dir(_)) => return Err(FsError::NotAFile),
            None if conf.create() || conf.create_new() => return Err(FsError::PermissionDenied),
            None => {
                if let Ok(mut cache) = self.negative_lookup_cache.lock() {
                    cache.insert(normalized.clone());
                }
                return Err(FsError::EntryNotFound);
            }
        };

        Ok(Box::new(StaticFile::new(entry.bytes.clone())))
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    fn sample_tar_bytes() -> Vec<u8> {
        let mut buffer = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut buffer);

            let payload = b"x = 1\n";
            let mut file = tar::Header::new_gnu();
            file.set_entry_type(tar::EntryType::Regular);
            file.set_mode(0o644);
            file.set_size(payload.len() as u64);
            file.set_cksum();
            builder
                .append_data(
                    &mut file,
                    "lib/python/__init__.py",
                    Cursor::new(payload.as_slice()),
                )
                .unwrap();

            builder.finish().unwrap();
        }
        buffer
    }

    #[tokio::test]
    async fn caches_negative_lookups_and_reuses_owned_buffer() {
        let fs = IndexedArchiveFileSystem::from_tar_reader(Cursor::new(sample_tar_bytes())).unwrap();

        let mut file = fs
            .new_open_options()
            .read(true)
            .open("/lib/python/__init__.py")
            .unwrap();
        let mut content = String::new();
        file.read_to_string(&mut content).await.unwrap();
        assert_eq!(content, "x = 1\n");

        let _ = fs.new_open_options().read(true).open("/lib/python/missing.py");
        let _ = fs.new_open_options().read(true).open("/lib/python/missing.py");
        assert_eq!(fs.negative_cache_size(), 1);
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

mod zip {
    use futures::future::BoxFuture;
    use std::{
        collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
        ffi::OsString,
        io::{Cursor, Read},
        path::{Component, Path, PathBuf},
        sync::{Arc, Mutex},
    };
    use shared_buffer::OwnedBuffer;
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
        decompressed_cache: Arc<Mutex<DecompressedLruCache>>,
        negative_lookup_cache: Arc<Mutex<NegativeLookupCache>>,
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
    
    const DEFAULT_DECOMPRESSED_CACHE_BYTES: usize = 128 * 1024 * 1024;
    const DEFAULT_NEGATIVE_LOOKUP_ENTRIES: usize = 32 * 1024;
    
    #[derive(Debug, Default)]
    struct DecompressedLruCache {
        max_bytes: usize,
        total_bytes: usize,
        entries: HashMap<usize, OwnedBuffer>,
        order: VecDeque<usize>,
    }
    
    impl DecompressedLruCache {
        fn new(max_bytes: usize) -> Self {
            Self {
                max_bytes,
                ..Default::default()
            }
        }
    
        fn get(&mut self, index: usize) -> Option<OwnedBuffer> {
            let value = self.entries.get(&index).cloned()?;
            self.touch(index);
            Some(value)
        }
    
        fn put(&mut self, index: usize, data: OwnedBuffer) {
            let data_len = data.len();
            if let Some(old) = self.entries.insert(index, data) {
                self.total_bytes = self.total_bytes.saturating_sub(old.len());
                self.remove_from_order(index);
            }
            self.total_bytes = self.total_bytes.saturating_add(data_len);
            self.order.push_back(index);
            self.evict_if_needed();
        }
    
        fn touch(&mut self, index: usize) {
            self.remove_from_order(index);
            self.order.push_back(index);
        }
    
        fn remove_from_order(&mut self, index: usize) {
            if let Some(pos) = self.order.iter().position(|v| *v == index) {
                let _ = self.order.remove(pos);
            }
        }
    
        fn evict_if_needed(&mut self) {
            while self.total_bytes > self.max_bytes {
                let Some(oldest) = self.order.pop_front() else {
                    break;
                };
                if let Some(old) = self.entries.remove(&oldest) {
                    self.total_bytes = self.total_bytes.saturating_sub(old.len());
                }
            }
        }
    }
    
    #[derive(Debug, Default)]
    struct NegativeLookupCache {
        max_entries: usize,
        entries: HashSet<PathBuf>,
        order: VecDeque<PathBuf>,
    }
    
    impl NegativeLookupCache {
        fn new(max_entries: usize) -> Self {
            Self {
                max_entries,
                ..Default::default()
            }
        }
    
        fn contains(&self, path: &Path) -> bool {
            self.entries.contains(path)
        }
    
        fn insert(&mut self, path: PathBuf) {
            if self.entries.insert(path.clone()) {
                self.order.push_back(path);
            }
            while self.entries.len() > self.max_entries {
                let Some(oldest) = self.order.pop_front() else {
                    break;
                };
                let _ = self.entries.remove(&oldest);
            }
        }
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
                decompressed_cache: Arc::new(Mutex::new(DecompressedLruCache::new(
                    DEFAULT_DECOMPRESSED_CACHE_BYTES,
                ))),
                negative_lookup_cache: Arc::new(Mutex::new(NegativeLookupCache::new(
                    DEFAULT_NEGATIVE_LOOKUP_ENTRIES,
                ))),
            })
        }
    
        fn node_for_path(&self, path: &Path) -> Result<(&PathBuf, &ZipNode), FsError> {
            let normalized = normalize_path(path, false)?;
            {
                let cache = self.negative_lookup_cache.lock().map_err(|_| FsError::Lock)?;
                if cache.contains(&normalized) {
                    return Err(FsError::EntryNotFound);
                }
            }
            self.nodes
                .get_key_value(&normalized)
                .ok_or_else(|| {
                    if let Ok(mut cache) = self.negative_lookup_cache.lock() {
                        cache.insert(normalized);
                    }
                    FsError::EntryNotFound
                })
        }
    
        #[cfg(test)]
        fn cache_state(&self) -> (usize, usize) {
            let dec = self.decompressed_cache.lock().unwrap();
            let neg = self.negative_lookup_cache.lock().unwrap();
            (dec.entries.len(), neg.entries.len())
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
    
            // Some runtimes may request write access even for read-mostly flows
            // (e.g. probing import paths). Keep archive filesystems read-only by
            // rejecting create/append/truncate semantics while still allowing open.
            if conf.create() || conf.create_new() || conf.append() || conf.truncate() {
                return Err(FsError::PermissionDenied);
            }
    
            let entry = match self.nodes.get(&normalized) {
                Some(ZipNode::File(file)) => file,
                Some(ZipNode::Dir(_)) => return Err(FsError::NotAFile),
                None if conf.create() || conf.create_new() => return Err(FsError::PermissionDenied),
                None => {
                    if let Ok(mut cache) = self.negative_lookup_cache.lock() {
                        cache.insert(normalized.clone());
                    }
                    return Err(FsError::EntryNotFound);
                }
            };
    
            if let Ok(mut cache) = self.decompressed_cache.lock() {
                if let Some(bytes) = cache.get(entry.index) {
                    return Ok(Box::new(StaticFile::new(bytes)));
                }
            }
    
            let mut archive = zip::ZipArchive::new(Cursor::new(self.archive_bytes.as_ref()))
                .map_err(map_zip_error)?;
            let mut zip_file = archive.by_index(entry.index).map_err(map_zip_error)?;
    
            let mut bytes = Vec::with_capacity(entry.metadata.len.min(usize::MAX as u64) as usize);
            zip_file.read_to_end(&mut bytes).map_err(FsError::from)?;
            let owned = OwnedBuffer::from(bytes);
            if let Ok(mut cache) = self.decompressed_cache.lock() {
                cache.put(entry.index, owned.clone());
            }
    
            Ok(Box::new(StaticFile::new(owned)))
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
                .append(true)
                .open("/lib/python/__init__.py")
                .unwrap_err();
            assert_eq!(err, FsError::PermissionDenied);
        }
    
        #[tokio::test]
        async fn caches_decompressed_content_and_negative_lookups() {
            let fs = ZipArchiveFileSystem::new(sample_zip_bytes()).unwrap();
    
            let mut file = fs
                .new_open_options()
                .read(true)
                .open("/lib/python/__init__.py")
                .unwrap();
            let mut content = String::new();
            file.read_to_string(&mut content).await.unwrap();
            assert_eq!(content, "x = 1\n");
    
            // Second read should hit decompressed LRU cache.
            let mut file = fs
                .new_open_options()
                .read(true)
                .open("/lib/python/__init__.py")
                .unwrap();
            let mut content = String::new();
            file.read_to_string(&mut content).await.unwrap();
            assert_eq!(content, "x = 1\n");
    
            let _ = fs.new_open_options().read(true).open("/lib/python/missing.py");
            let _ = fs.new_open_options().read(true).open("/lib/python/missing.py");
    
            let (decompressed_entries, negative_entries) = fs.cache_state();
            assert_eq!(decompressed_entries, 1);
            assert_eq!(negative_entries, 1);
        }
    }
}

pub use zip::ZipArchiveFileSystem;

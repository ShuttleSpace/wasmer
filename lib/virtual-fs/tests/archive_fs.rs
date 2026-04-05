use std::io::{Cursor, Write};
use std::path::Path;

use flate2::{Compression, write::GzEncoder};
use tempfile::NamedTempFile;
use tokio::io::AsyncReadExt;
use virtual_fs::{ArchiveFileSystem, ArchiveFormat, FileSystem, FsError};

fn sample_tar_bytes() -> Vec<u8> {
    let mut buffer = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut buffer);

        let mut root = tar::Header::new_gnu();
        root.set_entry_type(tar::EntryType::Directory);
        root.set_mode(0o755);
        root.set_size(0);
        root.set_cksum();
        builder
            .append_data(&mut root, "lib/", std::io::empty())
            .unwrap();

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

        let payload = b"print('ok')\n";
        let mut file = tar::Header::new_gnu();
        file.set_entry_type(tar::EntryType::Regular);
        file.set_mode(0o644);
        file.set_size(payload.len() as u64);
        file.set_cksum();
        builder
            .append_data(
                &mut file,
                "site-packages/pkg/mod.py",
                Cursor::new(payload.as_slice()),
            )
            .unwrap();

        builder.finish().unwrap();
    }

    buffer
}

fn sample_targz_bytes() -> Vec<u8> {
    let tar_bytes = sample_tar_bytes();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tar_bytes).unwrap();
    encoder.finish().unwrap()
}

fn sample_tarxz_bytes() -> Vec<u8> {
    let tar_bytes = sample_tar_bytes();
    let mut input = std::io::BufReader::new(Cursor::new(tar_bytes));
    let mut output = Vec::new();
    lzma_rs::xz_compress(&mut input, &mut output).unwrap();
    output
}

fn write_archive(bytes: &[u8], suffix: &str) -> NamedTempFile {
    let file = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
    std::fs::write(file.path(), bytes).unwrap();
    file
}

fn read_dir_paths(fs: &ArchiveFileSystem, path: &str) -> Vec<String> {
    let mut entries: Vec<_> = fs
        .read_dir(Path::new(path))
        .unwrap()
        .map(|entry| entry.unwrap().path.display().to_string())
        .collect();
    entries.sort();
    entries
}

#[test]
fn detect_format_from_suffix() {
    assert_eq!(
        ArchiveFormat::from_path(Path::new("/tmp/python3.zip")),
        Some(ArchiveFormat::Zip)
    );
    assert_eq!(
        ArchiveFormat::from_path(Path::new("/tmp/python3.tar")),
        Some(ArchiveFormat::Tar)
    );
    assert_eq!(
        ArchiveFormat::from_path(Path::new("/tmp/python3.tgz")),
        Some(ArchiveFormat::TarGz)
    );
    assert_eq!(
        ArchiveFormat::from_path(Path::new("/tmp/python3.tar.gz")),
        Some(ArchiveFormat::TarGz)
    );
    assert_eq!(
        ArchiveFormat::from_path(Path::new("/tmp/python3.txz")),
        Some(ArchiveFormat::TarXz)
    );
    assert_eq!(
        ArchiveFormat::from_path(Path::new("/tmp/python3.tar.xz")),
        Some(ArchiveFormat::TarXz)
    );
    assert_eq!(
        ArchiveFormat::from_path(Path::new("/tmp/python3.7z")),
        Some(ArchiveFormat::SevenZ)
    );
    assert_eq!(
        ArchiveFormat::from_path(Path::new("/tmp/python3.rar")),
        Some(ArchiveFormat::Rar)
    );
    assert_eq!(
        ArchiveFormat::from_path(Path::new("/tmp/python3.bin")),
        None
    );
}

#[tokio::test]
async fn reads_targz_archive() {
    let archive = write_archive(&sample_targz_bytes(), ".tar.gz");
    let fs = ArchiveFileSystem::from_path(archive.path()).unwrap();

    assert!(fs.metadata(Path::new("/lib/python")).unwrap().is_dir());
    assert_eq!(read_dir_paths(&fs, "/"), vec!["/lib", "/site-packages"]);

    let mut file = fs
        .new_open_options()
        .read(true)
        .open("/lib/python/__init__.py")
        .unwrap();
    let mut content = String::new();
    file.read_to_string(&mut content).await.unwrap();
    assert_eq!(content, "x = 1\n");

    let err = fs
        .new_open_options()
        .append(true)
        .open("/lib/python/__init__.py")
        .unwrap_err();
    assert_eq!(err, FsError::PermissionDenied);
}

#[tokio::test]
async fn reads_tarxz_archive() {
    let archive = write_archive(&sample_tarxz_bytes(), ".tar.xz");
    let fs = ArchiveFileSystem::from_path(archive.path()).unwrap();

    let mut file = fs
        .new_open_options()
        .read(true)
        .open("/site-packages/pkg/mod.py")
        .unwrap();
    let mut content = String::new();
    file.read_to_string(&mut content).await.unwrap();
    assert_eq!(content, "print('ok')\n");
}

//! Fingerprints and full digests. Reads bytes; never touches the catalog.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use md5::{Digest, Md5};
use xxhash_rust::xxh3::Xxh3;

use crate::model::Fingerprint;

/// XXH3-128 of the first [`HEAD`] bytes, or the whole file when it is
/// shorter. What ingest, scan, and acknowledge write. The size is compared
/// beside it, so it is not hashed. The window belongs to this name; a
/// different one is a new scheme name.
pub const HEAD_SCHEME: &str = "xxh3-128-head-256k";

/// XXH3-128 of the whole file, for files up to and including
/// [`SAMPLE_CUTOFF`]. No longer written; computed only to compare a new path
/// with a row that carries it.
pub const FULL_SCHEME: &str = "xxh3-128-full";

/// XXH3-128 of the file size as 8 little-endian bytes, then the first MiB, the
/// MiB centered on the midpoint, and the last MiB, for files over
/// [`SAMPLE_CUTOFF`]. No longer written, as for [`FULL_SCHEME`].
pub const SAMPLE_SCHEME: &str = "xxh3-128-sample-v1";

const KIB: u64 = 1 << 10;
const MIB: u64 = 1 << 20;
pub const HEAD: u64 = 256 * KIB;
pub const SAMPLE_CUTOFF: u64 = 64 * MIB;
const WINDOW: u64 = MIB;
const BUFFER: usize = 1 << 20;

/// What one full read of a file produced.
pub struct Digests {
    /// Lowercase hex, without the `blake3:` prefix.
    pub blake3: String,
    pub md5: Option<String>,
    pub fingerprint: Option<Fingerprint>,
}

/// The [`HEAD_SCHEME`] fingerprint: one read of at most [`HEAD`] bytes.
pub fn fingerprint(path: &Path) -> io::Result<Fingerprint> {
    let mut head = Vec::with_capacity(HEAD as usize);
    File::open(path)?.take(HEAD).read_to_end(&mut head)?;
    let mut hasher = Xxh3::new();
    hasher.update(&head);
    Ok(finish(HEAD_SCHEME, &hasher))
}

/// The fingerprint of a file whose stat reported `size`, in `scheme`. `None`
/// when this build cannot compute that scheme, or when no row of that scheme
/// can have this size.
pub fn fingerprint_in(scheme: &str, path: &Path, size: u64) -> io::Result<Option<Fingerprint>> {
    match scheme {
        HEAD_SCHEME => fingerprint(path).map(Some),
        FULL_SCHEME if size <= SAMPLE_CUTOFF => {
            let mut hasher = Xxh3::new();
            stream(&mut File::open(path)?, |chunk| hasher.update(chunk))?;
            Ok(Some(finish(FULL_SCHEME, &hasher)))
        }
        SAMPLE_SCHEME if size > SAMPLE_CUTOFF => sample(path, size).map(Some),
        _ => Ok(None),
    }
}

fn sample(path: &Path, size: u64) -> io::Result<Fingerprint> {
    let mut file = File::open(path)?;
    let mut hasher = Xxh3::new();
    hasher.update(&size.to_le_bytes());
    let middle = size / 2 - WINDOW / 2;
    for start in [0, middle, size - WINDOW] {
        file.seek(SeekFrom::Start(start))?;
        // A file that shrank since the stat gives a short window, not an error.
        let mut window = Vec::with_capacity(WINDOW as usize);
        (&mut file).take(WINDOW).read_to_end(&mut window)?;
        hasher.update(&window);
    }
    Ok(finish(SAMPLE_SCHEME, &hasher))
}

/// Read a file once for BLAKE3, and for MD5 when asked. When `with_fingerprint`
/// is set the [`HEAD_SCHEME`] fingerprint is taken from the same pass.
pub fn digest(path: &Path, md5: bool, with_fingerprint: bool) -> io::Result<Digests> {
    digest_reporting(path, md5, with_fingerprint, |_| {})
}

/// [`digest`], calling `on_read` with the length of each chunk of the full
/// read as it arrives.
pub fn digest_reporting(
    path: &Path,
    md5: bool,
    with_fingerprint: bool,
    mut on_read: impl FnMut(u64),
) -> io::Result<Digests> {
    let mut file = File::open(path)?;
    let mut blake3 = blake3::Hasher::new();
    let mut md5 = md5.then(Md5::new);
    let mut head = with_fingerprint.then(Xxh3::new);
    let mut head_left = HEAD as usize;
    stream(&mut file, |chunk| {
        on_read(chunk.len() as u64);
        blake3.update(chunk);
        if let Some(md5) = md5.as_mut() {
            md5.update(chunk);
        }
        if let Some(head) = head.as_mut() {
            let take = head_left.min(chunk.len());
            head.update(&chunk[..take]);
            head_left -= take;
        }
    })?;
    Ok(Digests {
        blake3: blake3.finalize().to_hex().to_string(),
        md5: md5.map(|md5| hex(&md5.finalize())),
        fingerprint: head.map(|head| finish(HEAD_SCHEME, &head)),
    })
}

fn stream(file: &mut File, mut each: impl FnMut(&[u8])) -> io::Result<()> {
    let mut buffer = vec![0; BUFFER];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => each(&buffer[..read]),
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

/// XXH3-128 in its canonical big-endian hex form.
fn finish(scheme: &str, hasher: &Xxh3) -> Fingerprint {
    Fingerprint {
        scheme: scheme.to_string(),
        hex: format!("{:032x}", hasher.digest128()),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use xxhash_rust::xxh3::xxh3_128;

    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bpm3-hash-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn a_small_file_is_hashed_in_full() {
        let path = scratch("small");
        fs::write(&path, b"ACGT\n").unwrap();
        let print = fingerprint(&path).unwrap();
        assert_eq!(print.scheme, HEAD_SCHEME);
        assert_eq!(print.hex, format!("{:032x}", xxh3_128(b"ACGT\n")));
        let digests = digest(&path, true, true).unwrap();
        assert_eq!(digests.fingerprint, Some(print));
        assert_eq!(digests.blake3, blake3::hash(b"ACGT\n").to_hex().to_string());
        assert_eq!(
            digests.md5.as_deref(),
            Some("58ce66d7df0a1cf9b360cabf43da3ea5")
        );
        let full = fingerprint_in(FULL_SCHEME, &path, 5).unwrap().unwrap();
        assert_eq!(full.hex, format!("{:032x}", xxh3_128(b"ACGT\n")));
        assert_eq!(fingerprint_in(SAMPLE_SCHEME, &path, 5).unwrap(), None);
        assert_eq!(fingerprint_in("other", &path, 5).unwrap(), None);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// Pins `xxh3-128-head-256k` to its window: the first 256 KiB only, the
    /// same whether read alone or taken from a full read.
    #[test]
    fn the_head_scheme_reads_only_the_first_256_kib() {
        let path = scratch("head");
        let mut bytes: Vec<u8> = (0..HEAD + 3 * BUFFER as u64 + 17)
            .map(|index| (index % 251) as u8)
            .collect();
        fs::write(&path, &bytes).unwrap();
        let print = fingerprint(&path).unwrap();
        assert_eq!(print.scheme, HEAD_SCHEME);
        assert_eq!(
            print.hex,
            format!("{:032x}", xxh3_128(&bytes[..HEAD as usize]))
        );
        assert_eq!(
            digest(&path, false, true).unwrap().fingerprint,
            Some(print.clone())
        );
        assert_eq!(digest(&path, false, false).unwrap().fingerprint, None);

        // A byte past the window does not change it; the last byte in it does.
        bytes[HEAD as usize] ^= 1;
        fs::write(&path, &bytes).unwrap();
        assert_eq!(fingerprint(&path).unwrap(), print);
        bytes[HEAD as usize - 1] ^= 1;
        fs::write(&path, &bytes).unwrap();
        assert_ne!(fingerprint(&path).unwrap(), print);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    /// Pins `xxh3-128-sample-v1` to its layout: size, first MiB, middle MiB,
    /// last MiB. Changing any of these needs a new scheme name.
    #[test]
    fn the_sample_scheme_layout_is_pinned() {
        let path = scratch("large");
        let size = SAMPLE_CUTOFF + 3 * MIB + 17;
        let file = fs::File::create(&path).unwrap();
        file.set_len(size).unwrap();
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        let marks = [
            (0, b"first"),
            (size / 2 - MIB / 2, b"midst"),
            (size - MIB, b"lasts"),
            (size / 4, b"unrd!"),
        ];
        for (offset, mark) in marks {
            file.seek(SeekFrom::Start(offset)).unwrap();
            file.write_all(mark).unwrap();
        }
        drop(file);

        let mut sample = size.to_le_bytes().to_vec();
        for (_, mark) in &marks[..3] {
            let mut window = vec![0u8; MIB as usize];
            window[..5].copy_from_slice(*mark);
            sample.extend_from_slice(&window);
        }
        let print = fingerprint_in(SAMPLE_SCHEME, &path, size).unwrap().unwrap();
        assert_eq!(print.scheme, SAMPLE_SCHEME);
        assert_eq!(print.hex, format!("{:032x}", xxh3_128(&sample)));

        // Bytes outside the three windows do not change the sample.
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(size / 4)).unwrap();
        file.write_all(b"other").unwrap();
        drop(file);
        assert_eq!(
            fingerprint_in(SAMPLE_SCHEME, &path, size).unwrap(),
            Some(print)
        );
        assert_eq!(fingerprint_in(FULL_SCHEME, &path, size).unwrap(), None);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}

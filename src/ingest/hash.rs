//! Fingerprints and full digests. Reads bytes; never touches the catalog.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use md5::{Digest, Md5};
use xxhash_rust::xxh3::Xxh3;

use crate::model::Fingerprint;

/// XXH3-128 of the whole file. Used up to and including [`SAMPLE_CUTOFF`].
pub const FULL_SCHEME: &str = "xxh3-128-full";

/// XXH3-128 of the file size as 8 little-endian bytes, then the first MiB, the
/// MiB centered on the midpoint, and the last MiB. The cutoff and the windows
/// belong to this name; a different layout is a new scheme name.
pub const SAMPLE_SCHEME: &str = "xxh3-128-sample-v1";

const MIB: u64 = 1 << 20;
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

/// The fingerprint of a file whose stat reported `size`. A file at or under
/// the cutoff is read in full. A larger one is sampled.
pub fn fingerprint(path: &Path, size: u64) -> io::Result<Fingerprint> {
    let mut file = File::open(path)?;
    if size <= SAMPLE_CUTOFF {
        let mut hasher = Xxh3::new();
        stream(&mut file, |chunk| hasher.update(chunk))?;
        return Ok(finish(FULL_SCHEME, &hasher));
    }
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
/// is set the fingerprint is computed too, in the same pass for a file the
/// full scheme covers.
pub fn digest(path: &Path, size: u64, md5: bool, with_fingerprint: bool) -> io::Result<Digests> {
    let mut file = File::open(path)?;
    let mut blake3 = blake3::Hasher::new();
    let mut md5 = md5.then(Md5::new);
    let mut xxh3 = (with_fingerprint && size <= SAMPLE_CUTOFF).then(Xxh3::new);
    stream(&mut file, |chunk| {
        blake3.update(chunk);
        if let Some(md5) = md5.as_mut() {
            md5.update(chunk);
        }
        if let Some(xxh3) = xxh3.as_mut() {
            xxh3.update(chunk);
        }
    })?;
    let fingerprint = match xxh3 {
        Some(xxh3) => Some(finish(FULL_SCHEME, &xxh3)),
        None if with_fingerprint => Some(fingerprint(path, size)?),
        None => None,
    };
    Ok(Digests {
        blake3: blake3.finalize().to_hex().to_string(),
        md5: md5.map(|md5| hex(&md5.finalize())),
        fingerprint,
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
        let print = fingerprint(&path, 5).unwrap();
        assert_eq!(print.scheme, FULL_SCHEME);
        assert_eq!(print.hex, format!("{:032x}", xxh3_128(b"ACGT\n")));
        // The cutoff itself is still the full scheme.
        let digests = digest(&path, 5, true, true).unwrap();
        assert_eq!(digests.fingerprint, Some(print));
        assert_eq!(digests.blake3, blake3::hash(b"ACGT\n").to_hex().to_string());
        assert_eq!(
            digests.md5.as_deref(),
            Some("58ce66d7df0a1cf9b360cabf43da3ea5")
        );
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
        let print = fingerprint(&path, size).unwrap();
        assert_eq!(print.scheme, SAMPLE_SCHEME);
        assert_eq!(print.hex, format!("{:032x}", xxh3_128(&sample)));

        // Bytes outside the three windows do not change the sample.
        let mut file = fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.seek(SeekFrom::Start(size / 4)).unwrap();
        file.write_all(b"other").unwrap();
        drop(file);
        assert_eq!(fingerprint(&path, size).unwrap(), print);
        assert_eq!(
            digest(&path, size, false, true).unwrap().fingerprint,
            Some(print)
        );
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}

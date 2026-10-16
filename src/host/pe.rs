//! Minimal PE (Windows DLL) reader for PiPL discovery.
//!
//! Reading resources straight from the file lets the plug-in catalog work
//! without loading any foreign code, and on Linux, where the DLLs can only
//! run under Wine, it is the only option.
//!
//! The files are untrusted: every read is bounds checked and malformed
//! images produce a [`PeError`] instead of a panic.

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

/// `IMAGE_FILE_MACHINE_I386`.
pub const MACHINE_I386: u16 = 0x014C;
/// `IMAGE_FILE_MACHINE_AMD64`.
pub const MACHINE_AMD64: u16 = 0x8664;
/// `IMAGE_FILE_MACHINE_ARM64`.
pub const MACHINE_ARM64: u16 = 0xAA64;

/// The architecture name returned for machine types the host can't run.
pub const UNKNOWN_ARCH: &str = "unknown";

/// The index of the resource table in the optional header's data directories.
const RESOURCE_DIRECTORY_INDEX: u32 = 2;

const DOS_MAGIC: &[u8; 2] = b"MZ";
const DOS_HEADER_SIZE: usize = 0x40;
/// Offset of `e_lfanew`, the file offset of the PE signature.
const PE_OFFSET_FIELD: usize = 0x3C;
const PE_SIGNATURE: &[u8; 4] = b"PE\0\0";
/// The PE signature plus the COFF file header.
const PE_HEADERS_SIZE: usize = 24;
const OPTIONAL_MAGIC_PE32: u16 = 0x10B;
const OPTIONAL_MAGIC_PE32_PLUS: u16 = 0x20B;
/// Offset of the data directories in a PE32 optional header.
const DIRECTORIES_PE32: usize = 96;
/// Offset of the data directories in a PE32+ optional header.
const DIRECTORIES_PE32_PLUS: usize = 112;
/// `NumberOfRvaAndSizes` sits just before the data directories.
const DIRECTORY_COUNT_SIZE: usize = 4;
const DATA_DIRECTORY_SIZE: usize = 8;
const SECTION_HEADER_SIZE: usize = 40;
const SECTION_NAME_SIZE: usize = 8;
const RESOURCE_DIRECTORY_SIZE: usize = 16;
/// Offset of the named and id entry counts in a resource directory.
const RESOURCE_DIRECTORY_COUNTS: usize = 12;
const RESOURCE_ENTRY_SIZE: usize = 8;
/// High bit of a resource entry field: a name offset or a subdirectory.
const RESOURCE_HIGH_BIT: u32 = 0x8000_0000;
const RESOURCE_OFFSET_MASK: u32 = 0x7FFF_FFFF;
const RESOURCE_ID_MASK: u32 = 0xFFFF;
/// More resources of one type than this means the tree is hostile: the
/// directories can point at each other and fan out to billions of entries.
pub const MAX_RESOURCES: usize = 4096;

/// The largest plug-in file read for its resources. Real filters are tens
/// of megabytes; this only stops a scan from reading something absurd.
pub const MAX_PE_FILE: u64 = 1024 * 1024 * 1024;
/// Resource names longer than this are cut short. Windows resource names
/// are short identifiers such as `PIPL`.
const MAX_RESOURCE_NAME_UNITS: usize = 256;

/// Raised for files that are not valid PE images.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PeError(pub String);

impl From<std::io::Error> for PeError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

/// Returns the plug-in architecture name for a PE machine type.
pub fn machine_arch(machine: u16) -> &'static str {
    match machine {
        MACHINE_I386 => "win-x86",
        MACHINE_AMD64 => "win-x64",
        MACHINE_ARM64 => "win-arm64",
        _ => UNKNOWN_ARCH,
    }
}

/// A resource type or name: numbered, or a string compared without case.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ResourceKey {
    Id(u16),
    Name(String),
}

impl ResourceKey {
    fn matches(&self, wanted: &ResourceKey) -> bool {
        match (self, wanted) {
            (Self::Id(a), Self::Id(b)) => a == b,
            (Self::Name(a), Self::Name(b)) => a.to_uppercase() == b.to_uppercase(),
            _ => false,
        }
    }
}

/// One section header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// The section name without trailing NULs.
    pub name: Vec<u8>,
    pub virtual_address: u32,
    /// The larger of the virtual and raw sizes.
    pub size: u32,
    pub raw_pointer: u32,
    pub raw_size: u32,
}

/// One entry of a resource directory.
struct DirectoryEntry {
    key: ResourceKey,
    is_dir: bool,
    target: usize,
}

fn read_array<const N: usize>(data: &[u8], pos: usize) -> Option<[u8; N]> {
    data.get(pos..pos.checked_add(N)?)?.try_into().ok()
}

fn read_u16(data: &[u8], pos: usize) -> Result<u16, PeError> {
    read_array(data, pos)
        .map(u16::from_le_bytes)
        .ok_or_else(|| PeError(format!("read past the end of the file at 0x{pos:x}")))
}

fn read_u32(data: &[u8], pos: usize) -> Result<u32, PeError> {
    read_array(data, pos)
        .map(u32::from_le_bytes)
        .ok_or_else(|| PeError(format!("read past the end of the file at 0x{pos:x}")))
}

fn offset(base: usize, delta: usize) -> Result<usize, PeError> {
    base.checked_add(delta)
        .ok_or_else(|| PeError("offset overflows".into()))
}

/// A parsed PE image: machine type, sections and resources.
#[derive(Clone, Debug)]
pub struct PeFile {
    data: Vec<u8>,
    /// The COFF machine type.
    pub machine: u16,
    /// RVA of the resource table, or 0 when there is none.
    pub resource_rva: u32,
    /// Size of the resource table.
    pub resource_size: u32,
    /// The section table.
    pub sections: Vec<Section>,
}

impl PeFile {
    /// Parses the headers of a PE image held in memory.
    ///
    /// # Errors
    ///
    /// Returns [`PeError`] when the DOS or PE signature is missing, the
    /// optional header magic is unknown, or the headers are truncated.
    pub fn parse(data: Vec<u8>) -> Result<Self, PeError> {
        if data.len() < DOS_HEADER_SIZE || !data.starts_with(DOS_MAGIC) {
            return Err(PeError("missing MZ header".into()));
        }
        let pe_offset = read_u32(&data, PE_OFFSET_FIELD)? as usize;
        let signature_ok = pe_offset
            .checked_add(PE_HEADERS_SIZE)
            .is_some_and(|end| end <= data.len())
            && data
                .get(pe_offset..)
                .is_some_and(|rest| rest.starts_with(PE_SIGNATURE));
        if !signature_ok {
            return Err(PeError("missing PE signature".into()));
        }
        let file_header = offset(pe_offset, PE_SIGNATURE.len())?;
        let machine = read_u16(&data, file_header)?;
        let section_count = read_u16(&data, offset(file_header, 2)?)?;
        let optional_size = read_u16(&data, offset(file_header, 16)?)?;
        let optional = offset(pe_offset, PE_HEADERS_SIZE)?;
        let directories = match read_u16(&data, optional)? {
            OPTIONAL_MAGIC_PE32 => offset(optional, DIRECTORIES_PE32)?,
            OPTIONAL_MAGIC_PE32_PLUS => offset(optional, DIRECTORIES_PE32_PLUS)?,
            magic => {
                return Err(PeError(format!(
                    "unknown optional header magic 0x{magic:x}"
                )))
            }
        };
        let directory_count = read_u32(&data, directories - DIRECTORY_COUNT_SIZE)?;
        let (resource_rva, resource_size) = if directory_count > RESOURCE_DIRECTORY_INDEX {
            let entry = offset(
                directories,
                RESOURCE_DIRECTORY_INDEX as usize * DATA_DIRECTORY_SIZE,
            )?;
            (read_u32(&data, entry)?, read_u32(&data, offset(entry, 4)?)?)
        } else {
            (0, 0)
        };

        let section_table = offset(optional, usize::from(optional_size))?;
        let mut sections = Vec::with_capacity(usize::from(section_count));
        for index in 0..usize::from(section_count) {
            let start = offset(section_table, index * SECTION_HEADER_SIZE)?;
            let Some(header) = data
                .get(start..)
                .and_then(|rest| rest.get(..SECTION_HEADER_SIZE))
            else {
                return Err(PeError("section table is truncated".into()));
            };
            let name_end = header[..SECTION_NAME_SIZE]
                .iter()
                .rposition(|&b| b != 0)
                .map_or(0, |last| last + 1);
            let virtual_size = read_u32(header, 8)?;
            let virtual_address = read_u32(header, 12)?;
            let raw_size = read_u32(header, 16)?;
            let raw_pointer = read_u32(header, 20)?;
            sections.push(Section {
                name: header[..name_end].to_vec(),
                virtual_address,
                size: virtual_size.max(raw_size),
                raw_pointer,
                raw_size,
            });
        }
        Ok(Self {
            data,
            machine,
            resource_rva,
            resource_size,
            sections,
        })
    }

    /// Reads and parses a PE file.
    ///
    /// # Errors
    ///
    /// Returns [`PeError`] when the file can't be read or isn't a PE image.
    pub fn open(path: &Path) -> Result<Self, PeError> {
        Self::parse(super::discovery::read_capped(path, MAX_PE_FILE)?)
    }

    /// The plug-in architecture name, or `"unknown"`.
    pub fn arch(&self) -> &'static str {
        machine_arch(self.machine)
    }

    /// Converts a relative virtual address to a file offset.
    ///
    /// # Errors
    ///
    /// Returns [`PeError`] when the address is outside every section or
    /// points at data that isn't stored in the file.
    pub fn rva_to_offset(&self, rva: u32) -> Result<usize, PeError> {
        let section = self.sections.iter().find(|section| {
            let start = u64::from(section.virtual_address);
            let end = start + u64::from(section.size);
            (start..end).contains(&u64::from(rva))
        });
        let Some(section) = section else {
            return Err(PeError(format!("RVA 0x{rva:x} is outside every section")));
        };
        let delta = rva - section.virtual_address;
        if delta >= section.raw_size {
            return Err(PeError(format!(
                "RVA 0x{rva:x} points at uninitialised data"
            )));
        }
        offset(section.raw_pointer as usize, delta as usize)
    }

    fn directory_entries(
        &self,
        base: usize,
        relative: usize,
    ) -> Result<Vec<DirectoryEntry>, PeError> {
        let data = &self.data;
        let pos = offset(base, relative)?;
        if offset(pos, RESOURCE_DIRECTORY_SIZE)? > data.len() {
            return Err(PeError("resource directory is truncated".into()));
        }
        let named = read_u16(data, pos + RESOURCE_DIRECTORY_COUNTS)?;
        let ids = read_u16(data, pos + RESOURCE_DIRECTORY_COUNTS + 2)?;
        let count = usize::from(named) + usize::from(ids);
        // Each entry may carry a name, so a hostile directory listing tens
        // of thousands of entries costs memory before any are matched.
        if count > MAX_RESOURCES {
            return Err(PeError("too many resources".into()));
        }
        let mut entries = Vec::new();
        for index in 0..count {
            let entry = offset(pos, RESOURCE_DIRECTORY_SIZE + index * RESOURCE_ENTRY_SIZE)?;
            let name_field = read_u32(data, entry)?;
            let target = read_u32(data, offset(entry, 4)?)?;
            let key =
                if name_field & RESOURCE_HIGH_BIT != 0 {
                    ResourceKey::Name(self.resource_name(offset(
                        base,
                        (name_field & RESOURCE_OFFSET_MASK) as usize,
                    )?)?)
                } else {
                    ResourceKey::Id((name_field & RESOURCE_ID_MASK) as u16)
                };
            entries.push(DirectoryEntry {
                key,
                is_dir: target & RESOURCE_HIGH_BIT != 0,
                target: (target & RESOURCE_OFFSET_MASK) as usize,
            });
        }
        Ok(entries)
    }

    /// Reads a length-prefixed UTF-16 resource name.
    ///
    /// Like the Python host, a name running past the end of the file is cut
    /// short and bad code units are replaced. Names are also cut at
    /// `MAX_RESOURCE_NAME_UNITS`.
    fn resource_name(&self, pos: usize) -> Result<String, PeError> {
        let length = usize::from(read_u16(&self.data, pos)?).min(MAX_RESOURCE_NAME_UNITS);
        let start = offset(pos, 2)?;
        let end = start.saturating_add(length * 2).min(self.data.len());
        let raw = self.data.get(start..end).unwrap_or_default();
        let units: Vec<u16> = raw
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        Ok(String::from_utf16_lossy(&units))
    }

    /// Returns `(name, bytes)` for every resource of a type.
    ///
    /// Name comparison ignores case, as Windows does. When a resource has
    /// several language variants, the first is used.
    ///
    /// # Errors
    ///
    /// Returns [`PeError`] when the resource tree is malformed, or when its
    /// entries overlap so that copying them would take more bytes than the
    /// file holds: a hostile tree can point thousands of entries at one
    /// large block.
    pub fn resources(
        &self,
        resource_type: &ResourceKey,
    ) -> Result<Vec<(ResourceKey, Vec<u8>)>, PeError> {
        let mut found = Vec::new();
        if self.resource_rva == 0 {
            return Ok(found);
        }
        let base = self.rva_to_offset(self.resource_rva)?;
        let mut visited = HashSet::new();
        let mut copied = 0usize;
        for type_entry in self.directory_entries(base, 0)? {
            if !type_entry.key.matches(resource_type) || !type_entry.is_dir {
                continue;
            }
            // Several type entries pointing at one name directory would
            // otherwise have it parsed again for each of them.
            if !visited.insert(type_entry.target) {
                continue;
            }
            for name_entry in self.directory_entries(base, type_entry.target)? {
                if found.len() >= MAX_RESOURCES {
                    return Err(PeError("too many resources".into()));
                }
                let data_entry = if name_entry.is_dir {
                    match self.directory_entries(base, name_entry.target)?.first() {
                        Some(language) => language.target,
                        None => continue,
                    }
                } else {
                    name_entry.target
                };
                let bytes = self.read_data_entry(base, data_entry)?;
                copied = copied.saturating_add(bytes.len());
                if copied > self.data.len() {
                    return Err(PeError("resources overlap".into()));
                }
                found.push((name_entry.key, bytes));
            }
        }
        Ok(found)
    }

    /// Reads the bytes a resource data entry points at.
    ///
    /// Like the Python host, data running past the end of the file is cut
    /// short; the PiPL parser then rejects it if it matters.
    fn read_data_entry(&self, base: usize, relative: usize) -> Result<Vec<u8>, PeError> {
        let pos = offset(base, relative)?;
        let rva = read_u32(&self.data, pos)?;
        let size = read_u32(&self.data, offset(pos, 4)?)? as usize;
        let start = self.rva_to_offset(rva)?;
        let end = start.saturating_add(size).min(self.data.len());
        Ok(self.data.get(start..end).unwrap_or_default().to_vec())
    }
}

/// Returns true when a file starts with the `MZ` signature.
///
/// Anything but a regular file is refused before it is opened: opening a
/// FIFO would block until a writer appears.
pub fn is_pe_file(path: &Path) -> bool {
    if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        return false;
    }
    let mut magic = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .is_ok_and(|()| &magic == DOS_MAGIC)
}

/// Builders for synthetic PE images used by the unit tests.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// File offset of the resource section in built images.
    pub const RSRC_FILE_OFFSET: usize = 0x200;
    /// RVA of the resource section in built images.
    pub const RSRC_RVA: u32 = 0x1000;
    const LANGUAGE_EN_US: u32 = 0x409;
    const DIRECTORY_COUNT: u32 = 16;

    fn push_u16(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn put_u32(out: &mut [u8], pos: usize, value: u32) {
        out[pos..pos + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn directory(named: u16, ids: u16) -> Vec<u8> {
        let mut out = vec![0; RESOURCE_DIRECTORY_COUNTS];
        push_u16(&mut out, named);
        push_u16(&mut out, ids);
        out
    }

    /// Builds the `.rsrc` section: a named type holding numbered resources,
    /// each with one language variant.
    pub fn build_resource_section(type_name: &str, resources: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let count = resources.len();
        let root_size = RESOURCE_DIRECTORY_SIZE + RESOURCE_ENTRY_SIZE;
        let type_dir = root_size;
        let type_size = RESOURCE_DIRECTORY_SIZE + count * RESOURCE_ENTRY_SIZE;
        let lang_dirs = type_dir + type_size;
        let lang_size = RESOURCE_DIRECTORY_SIZE + RESOURCE_ENTRY_SIZE;
        let data_entries = lang_dirs + count * lang_size;
        let name_pos = data_entries + count * 16;
        let units: Vec<u16> = type_name.encode_utf16().collect();
        let mut blobs = name_pos + 2 + units.len() * 2;
        blobs = (blobs + 3) & !3;

        let mut out = directory(1, 0);
        push_u32(&mut out, RESOURCE_HIGH_BIT | name_pos as u32);
        push_u32(&mut out, RESOURCE_HIGH_BIT | type_dir as u32);
        out.extend(directory(0, count as u16));
        for (index, (id, _)) in resources.iter().enumerate() {
            push_u32(&mut out, u32::from(*id));
            push_u32(
                &mut out,
                RESOURCE_HIGH_BIT | (lang_dirs + index * lang_size) as u32,
            );
        }
        for index in 0..count {
            out.extend(directory(0, 1));
            push_u32(&mut out, LANGUAGE_EN_US);
            push_u32(&mut out, (data_entries + index * 16) as u32);
        }
        let mut blob_offsets = Vec::new();
        let mut next = blobs;
        for (_, payload) in resources {
            blob_offsets.push(next);
            next = (next + payload.len() + 3) & !3;
        }
        for ((_, payload), blob) in resources.iter().zip(&blob_offsets) {
            push_u32(&mut out, RSRC_RVA + *blob as u32);
            push_u32(&mut out, payload.len() as u32);
            push_u32(&mut out, 0);
            push_u32(&mut out, 0);
        }
        push_u16(&mut out, units.len() as u16);
        for unit in units {
            push_u16(&mut out, unit);
        }
        for ((_, payload), blob) in resources.iter().zip(&blob_offsets) {
            out.resize(*blob, 0);
            out.extend_from_slice(payload);
        }
        out
    }

    /// Builds a PE image with one `.rsrc` section containing `section`.
    pub fn build_pe_with_section(machine: u16, pe32_plus: bool, section: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; DOS_HEADER_SIZE];
        out[..2].copy_from_slice(DOS_MAGIC);
        put_u32(&mut out, PE_OFFSET_FIELD, DOS_HEADER_SIZE as u32);
        out.extend_from_slice(PE_SIGNATURE);
        let (magic, directories) = if pe32_plus {
            (OPTIONAL_MAGIC_PE32_PLUS, DIRECTORIES_PE32_PLUS)
        } else {
            (OPTIONAL_MAGIC_PE32, DIRECTORIES_PE32)
        };
        let optional_size = directories + DIRECTORY_COUNT as usize * DATA_DIRECTORY_SIZE;
        push_u16(&mut out, machine);
        push_u16(&mut out, 1);
        out.extend_from_slice(&[0; 12]);
        push_u16(&mut out, optional_size as u16);
        push_u16(&mut out, 0);
        let optional = out.len();
        out.resize(optional + optional_size, 0);
        out[optional..optional + 2].copy_from_slice(&magic.to_le_bytes());
        put_u32(
            &mut out,
            optional + directories - DIRECTORY_COUNT_SIZE,
            DIRECTORY_COUNT,
        );
        let resource_entry =
            optional + directories + RESOURCE_DIRECTORY_INDEX as usize * DATA_DIRECTORY_SIZE;
        put_u32(&mut out, resource_entry, RSRC_RVA);
        put_u32(&mut out, resource_entry + 4, section.len() as u32);
        let mut header = vec![0u8; SECTION_HEADER_SIZE];
        header[..5].copy_from_slice(b".rsrc");
        put_u32(&mut header, 8, section.len() as u32);
        put_u32(&mut header, 12, RSRC_RVA);
        put_u32(&mut header, 16, section.len() as u32);
        put_u32(&mut header, 20, RSRC_FILE_OFFSET as u32);
        out.extend(header);
        out.resize(RSRC_FILE_OFFSET, 0);
        out.extend_from_slice(section);
        out
    }

    /// Builds an x64 PE image with `PIPL` resources.
    pub fn build_pe(resources: &[(u16, Vec<u8>)]) -> Vec<u8> {
        build_pe_with_section(
            MACHINE_AMD64,
            true,
            &build_resource_section("PIPL", resources),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    fn pipl_type() -> ResourceKey {
        ResourceKey::Name("PiPL".into())
    }

    #[test]
    fn error_for_garbage() {
        assert!(PeFile::parse(b"not a dll".to_vec()).is_err());
        assert!(PeFile::parse(Vec::new()).is_err());
    }

    #[test]
    fn reads_resources_from_a_pe32_plus_image() {
        let image = PeFile::parse(build_pe(&[
            (16000, b"one".to_vec()),
            (16001, b"second".to_vec()),
        ]))
        .unwrap();
        assert_eq!(image.arch(), "win-x64");
        assert_eq!(image.sections.len(), 1);
        assert_eq!(image.sections[0].name, b".rsrc");
        let resources = image.resources(&pipl_type()).unwrap();
        assert_eq!(
            resources,
            vec![
                (ResourceKey::Id(16000), b"one".to_vec()),
                (ResourceKey::Id(16001), b"second".to_vec()),
            ]
        );
        assert!(image
            .resources(&ResourceKey::Name("ICON".into()))
            .unwrap()
            .is_empty());
        assert!(image.resources(&ResourceKey::Id(10)).unwrap().is_empty());
    }

    #[test]
    fn reads_a_pe32_image() {
        let section = build_resource_section("pipl", &[(1, b"x".to_vec())]);
        let image = PeFile::parse(build_pe_with_section(MACHINE_I386, false, &section)).unwrap();
        assert_eq!(image.arch(), "win-x86");
        assert_eq!(image.resources(&pipl_type()).unwrap().len(), 1);
    }

    #[test]
    fn machine_names() {
        assert_eq!(machine_arch(MACHINE_ARM64), "win-arm64");
        assert_eq!(machine_arch(0x1234), UNKNOWN_ARCH);
    }

    #[test]
    fn image_without_resources() {
        let mut data = build_pe(&[]);
        // Zero NumberOfRvaAndSizes: no resource directory at all.
        let count =
            DOS_HEADER_SIZE + PE_HEADERS_SIZE + DIRECTORIES_PE32_PLUS - DIRECTORY_COUNT_SIZE;
        data[count..count + 4].copy_from_slice(&0u32.to_le_bytes());
        let image = PeFile::parse(data).unwrap();
        assert_eq!(image.resource_rva, 0);
        assert!(image.resources(&pipl_type()).unwrap().is_empty());
    }

    #[test]
    fn malformed_headers() {
        let good = build_pe(&[(1, b"x".to_vec())]);
        let mut bad_offset = good.clone();
        bad_offset[PE_OFFSET_FIELD..PE_OFFSET_FIELD + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut bad_signature = good.clone();
        bad_signature[DOS_HEADER_SIZE] = b'X';
        let mut bad_magic = good.clone();
        bad_magic[DOS_HEADER_SIZE + PE_HEADERS_SIZE] = 0;
        let mut many_sections = good.clone();
        many_sections[DOS_HEADER_SIZE + 6..DOS_HEADER_SIZE + 8]
            .copy_from_slice(&u16::MAX.to_le_bytes());
        let cases = [
            ("dos only", good[..DOS_HEADER_SIZE].to_vec()),
            ("pe header truncated", good[..DOS_HEADER_SIZE + 30].to_vec()),
            ("optional truncated", good[..DOS_HEADER_SIZE + 100].to_vec()),
            ("bad offset", bad_offset),
            ("bad signature", bad_signature),
            ("bad magic", bad_magic),
            ("section table truncated", many_sections),
        ];
        for (label, data) in cases {
            assert!(PeFile::parse(data).is_err(), "{label} should fail");
        }
    }

    #[test]
    fn malformed_resource_trees() {
        let good = build_pe(&[(1, b"x".to_vec())]);
        let root = RSRC_FILE_OFFSET;
        // Truncated section: the resource directory runs past the file.
        let truncated = PeFile::parse(good[..root + 8].to_vec()).unwrap();
        assert!(truncated.resources(&pipl_type()).is_err());
        // The type entry points at a directory far outside the file.
        let mut bad_target = good.clone();
        let target = root + RESOURCE_DIRECTORY_SIZE + 4;
        bad_target[target..target + 4]
            .copy_from_slice(&(RESOURCE_HIGH_BIT | 0x7FFF_0000).to_le_bytes());
        assert!(PeFile::parse(bad_target)
            .unwrap()
            .resources(&pipl_type())
            .is_err());
        // An absurd entry count runs off the end.
        let mut many = good.clone();
        many[root + 14..root + 16].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(PeFile::parse(many)
            .unwrap()
            .resources(&ResourceKey::Id(1))
            .is_err());
        // The resource RVA is outside every section.
        let mut image = PeFile::parse(good.clone()).unwrap();
        image.resource_rva = 0x9000;
        assert!(image.resources(&pipl_type()).is_err());
        // A section whose raw size is smaller than the RVA delta.
        let mut image = PeFile::parse(good).unwrap();
        image.sections[0].raw_size = 4;
        image.resource_rva = RSRC_RVA + 8;
        assert!(image.resources(&pipl_type()).is_err());
    }

    #[test]
    fn resource_fan_out_is_capped() {
        // A numbered type whose directory lists MAX_RESOURCES + 1 entries,
        // all pointing at the same data entry.
        let count = MAX_RESOURCES + 1;
        let type_dir = RESOURCE_DIRECTORY_SIZE + RESOURCE_ENTRY_SIZE;
        let data_entry = type_dir + RESOURCE_DIRECTORY_SIZE + count * RESOURCE_ENTRY_SIZE;
        let mut section = vec![0u8; RESOURCE_DIRECTORY_COUNTS + 2];
        section.extend_from_slice(&1u16.to_le_bytes());
        section.extend_from_slice(&10u32.to_le_bytes());
        section.extend_from_slice(&(RESOURCE_HIGH_BIT | type_dir as u32).to_le_bytes());
        section.extend_from_slice(&[0; RESOURCE_DIRECTORY_COUNTS + 2]);
        section.extend_from_slice(&(count as u16).to_le_bytes());
        for id in 0..count {
            section.extend_from_slice(&(id as u32).to_le_bytes());
            section.extend_from_slice(&(data_entry as u32).to_le_bytes());
        }
        section.extend_from_slice(&RSRC_RVA.to_le_bytes());
        section.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let image = PeFile::parse(build_pe_with_section(MACHINE_AMD64, true, &section)).unwrap();
        let error = image.resources(&ResourceKey::Id(10)).unwrap_err();
        assert_eq!(error.0, "too many resources");
    }

    #[test]
    fn overlapping_resources_are_refused() {
        // Many entries all pointing at one data entry that claims the
        // whole section: copying each would multiply the file's size.
        let count = 256;
        let type_dir = RESOURCE_DIRECTORY_SIZE + RESOURCE_ENTRY_SIZE;
        let data_entry = type_dir + RESOURCE_DIRECTORY_SIZE + count * RESOURCE_ENTRY_SIZE;
        let mut section = vec![0u8; RESOURCE_DIRECTORY_COUNTS + 2];
        section.extend_from_slice(&1u16.to_le_bytes());
        section.extend_from_slice(&10u32.to_le_bytes());
        section.extend_from_slice(&(RESOURCE_HIGH_BIT | type_dir as u32).to_le_bytes());
        section.extend_from_slice(&[0; RESOURCE_DIRECTORY_COUNTS + 2]);
        section.extend_from_slice(&(count as u16).to_le_bytes());
        for id in 0..count {
            section.extend_from_slice(&(id as u32).to_le_bytes());
            section.extend_from_slice(&(data_entry as u32).to_le_bytes());
        }
        section.extend_from_slice(&RSRC_RVA.to_le_bytes());
        section.extend_from_slice(&u32::MAX.to_le_bytes());
        section.extend_from_slice(&[0; 8]);
        let image = PeFile::parse(build_pe_with_section(MACHINE_AMD64, true, &section)).unwrap();
        let error = image.resources(&ResourceKey::Id(10)).unwrap_err();
        assert_eq!(error.0, "resources overlap");
    }

    #[test]
    fn overlong_data_is_cut_short() {
        let mut data = build_pe(&[(1, b"abcd".to_vec())]);
        let len = data.len();
        data.truncate(len - 2);
        let image = PeFile::parse(data).unwrap();
        assert_eq!(image.resources(&pipl_type()).unwrap()[0].1, b"ab");
    }

    #[test]
    fn detects_mz_files() {
        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join("a.8bf");
        std::fs::write(&dll, b"MZ").unwrap();
        let text = dir.path().join("b.8bf");
        std::fs::write(&text, b"M").unwrap();
        assert!(is_pe_file(&dll));
        assert!(!is_pe_file(&text));
        assert!(!is_pe_file(&dir.path().join("missing")));
        assert!(PeFile::open(&dir.path().join("missing")).is_err());
    }
}

//! Reader for classic Mac OS resource files and resource forks.
//!
//! macOS Photoshop plug-ins keep their PiPL in a resource map, either in a
//! `Contents/Resources/*.rsrc` file inside the bundle or in the resource
//! fork of the executable. Both use the same big-endian format:
//!
//! ```text
//! header:  uint32 dataOffset, mapOffset, dataLength, mapLength
//! map:     16-byte header copy, uint32 handle, uint16 fileRef,
//!          uint16 attributes, uint16 typeListOffset, uint16 nameListOffset
//! types:   uint16 typeCount-1, then {OSType, uint16 count-1, uint16 refOffset}
//! refs:    {int16 id, uint16 nameOffset, uint8 attrs, uint24 dataOffset, uint32}
//! data:    uint32 length followed by the bytes
//! ```
//!
//! The data is untrusted: every read is bounds checked and malformed maps
//! produce a [`ResourceForkError`] instead of a panic.

const HEADER_SIZE: usize = 16;
/// The smallest map: header copy, handle, file ref, attributes and offsets.
const MIN_MAP_SIZE: usize = 28;
/// Offset of the type list offset inside the map.
const TYPE_LIST_OFFSET_FIELD: usize = 24;
const TYPE_ENTRY_SIZE: usize = 8;
const REFERENCE_SIZE: usize = 12;
/// The low 24 bits of a reference's packed word are the data offset.
const DATA_OFFSET_MASK: u32 = 0x00FF_FFFF;
/// Each resource's data starts with a 4-byte length.
const DATA_LENGTH_SIZE: usize = 4;
/// "No name" in a reference's name offset.
const NO_NAME: u16 = 0xFFFF;
/// Where [`build_resource_file`] puts the data section.
const BUILT_DATA_OFFSET: usize = 256;

pub use super::pe::MAX_RESOURCES;

/// One resource: its id and its bytes.
pub type Resource = (i16, Vec<u8>);

/// Raised when data is not a valid resource map.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ResourceForkError(pub String);

fn read_array<const N: usize>(data: &[u8], pos: usize) -> Option<[u8; N]> {
    data.get(pos..pos.checked_add(N)?)?.try_into().ok()
}

fn read_u16(data: &[u8], pos: usize, what: &str) -> Result<u16, ResourceForkError> {
    read_array(data, pos)
        .map(u16::from_be_bytes)
        .ok_or_else(|| ResourceForkError(format!("{what} is truncated")))
}

fn read_u32(data: &[u8], pos: usize, what: &str) -> Result<u32, ResourceForkError> {
    read_array(data, pos)
        .map(u32::from_be_bytes)
        .ok_or_else(|| ResourceForkError(format!("{what} is truncated")))
}

fn fits(start: u32, length: u32, total: usize) -> bool {
    (u64::from(start) + u64::from(length)) <= total as u64
}

/// Returns `(resource_id, bytes)` for every resource of one type.
///
/// Like the Python host, data whose length runs past the end of the data
/// section is cut short rather than rejected.
///
/// # Errors
///
/// Returns [`ResourceForkError`] when the header is inconsistent or the
/// type list, reference list or a data offset is out of range, when there
/// are more than [`MAX_RESOURCES`] of the type, or when the resources
/// overlap so that copying them would exceed the size of the data section.
pub fn read_resources(
    data: &[u8],
    resource_type: &[u8; 4],
) -> Result<Vec<Resource>, ResourceForkError> {
    if data.len() < HEADER_SIZE {
        return Err(ResourceForkError("resource data is too short".into()));
    }
    let data_offset = read_u32(data, 0, "resource header")?;
    let map_offset = read_u32(data, 4, "resource header")?;
    let data_length = read_u32(data, 8, "resource header")?;
    let map_length = read_u32(data, 12, "resource header")?;
    if !fits(data_offset, data_length, data.len())
        || !fits(map_offset, map_length, data.len())
        || (map_length as usize) < MIN_MAP_SIZE
    {
        return Err(ResourceForkError("resource header is inconsistent".into()));
    }
    let data_offset = data_offset as usize;
    let data_end = data_offset + data_length as usize;
    let map_offset = map_offset as usize;
    let type_list_offset = read_u16(data, map_offset + TYPE_LIST_OFFSET_FIELD, "resource map")?;
    let type_list = map_offset + usize::from(type_list_offset);
    let type_count = usize::from(read_u16(data, type_list, "resource type list")?.wrapping_add(1));

    let mut results = Vec::new();
    // Every reference can point at the same bytes, so without a budget a
    // small file expands into copies of its data section thousands of times
    // over. Real resources don't overlap, so their total is at most the
    // data section's length.
    let mut copied = 0usize;
    for index in 0..type_count {
        let entry = type_list + 2 + index * TYPE_ENTRY_SIZE;
        let Some(type_entry) = data.get(entry..entry + TYPE_ENTRY_SIZE) else {
            return Err(ResourceForkError("resource type list is truncated".into()));
        };
        if &type_entry[..4] != resource_type {
            continue;
        }
        let count = usize::from(read_u16(type_entry, 4, "resource type list")?) + 1;
        let ref_offset = usize::from(read_u16(type_entry, 6, "resource type list")?);
        for ref_index in 0..count {
            let reference = type_list + ref_offset + ref_index * REFERENCE_SIZE;
            if reference + REFERENCE_SIZE > data.len() {
                return Err(ResourceForkError(
                    "resource reference list is truncated".into(),
                ));
            }
            if results.len() >= MAX_RESOURCES {
                return Err(ResourceForkError("too many resources".into()));
            }
            let resource_id = read_u16(data, reference, "resource reference list")? as i16;
            let packed = read_u32(data, reference + 4, "resource reference list")?;
            let offset = data_offset + (packed & DATA_OFFSET_MASK) as usize;
            let section = data.get(..data_end).unwrap_or_default();
            let Ok(length) = read_u32(section, offset, "resource data") else {
                return Err(ResourceForkError(
                    "resource data offset is out of range".into(),
                ));
            };
            // Data running past the data section is cut short there.
            let start = offset + DATA_LENGTH_SIZE;
            let end = start.saturating_add(length as usize).min(data_end);
            let bytes = section.get(start..end).unwrap_or_default();
            copied = copied.saturating_add(bytes.len());
            if copied > data_length as usize {
                return Err(ResourceForkError(
                    "resources overlap: their total exceeds the data section".into(),
                ));
            }
            results.push((resource_id, bytes.to_vec()));
        }
    }
    Ok(results)
}

/// Builds a resource file from `(type, [(id, bytes), ...])` groups.
///
/// Used by the tests to create Mac-style fixtures. Data offsets are stored
/// in 24 bits, so the total payload must stay below 16 MiB.
pub fn build_resource_file(resources: &[([u8; 4], Vec<Resource>)]) -> Vec<u8> {
    let mut data_section = Vec::new();
    let mut type_entries = Vec::new();
    for (type_code, items) in resources {
        let mut refs = Vec::new();
        for (resource_id, payload) in items {
            refs.push((*resource_id, data_section.len() as u32));
            data_section.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            data_section.extend_from_slice(payload);
        }
        type_entries.push((type_code, refs));
    }

    let mut type_list = (type_entries.len() as u16)
        .wrapping_sub(1)
        .to_be_bytes()
        .to_vec();
    let mut ref_lists = Vec::new();
    let ref_base = 2 + type_entries.len() * TYPE_ENTRY_SIZE;
    for (type_code, refs) in &type_entries {
        type_list.extend_from_slice(*type_code);
        type_list.extend_from_slice(&(refs.len() as u16).wrapping_sub(1).to_be_bytes());
        type_list.extend_from_slice(&((ref_base + ref_lists.len()) as u16).to_be_bytes());
        for (resource_id, offset) in refs {
            ref_lists.extend_from_slice(&resource_id.to_be_bytes());
            ref_lists.extend_from_slice(&NO_NAME.to_be_bytes());
            ref_lists.extend_from_slice(&(offset & DATA_OFFSET_MASK).to_be_bytes());
            ref_lists.extend_from_slice(&[0; 4]);
        }
    }

    let map_offset = BUILT_DATA_OFFSET + data_section.len();
    let mut map_body = vec![0u8; TYPE_LIST_OFFSET_FIELD];
    map_body.extend_from_slice(&(MIN_MAP_SIZE as u16).to_be_bytes());
    map_body.extend_from_slice(
        &((MIN_MAP_SIZE + type_list.len() + ref_lists.len()) as u16).to_be_bytes(),
    );
    map_body.extend_from_slice(&type_list);
    map_body.extend_from_slice(&ref_lists);

    let mut out = Vec::new();
    for field in [
        BUILT_DATA_OFFSET,
        map_offset,
        data_section.len(),
        map_body.len(),
    ] {
        out.extend_from_slice(&(field as u32).to_be_bytes());
    }
    out.resize(BUILT_DATA_OFFSET, 0);
    out.extend_from_slice(&data_section);
    out.extend_from_slice(&map_body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        build_resource_file(&[
            (
                *b"PiPL",
                vec![(16000, b"one".to_vec()), (16001, b"two".to_vec())],
            ),
            (*b"STR ", vec![(1, b"x".to_vec())]),
        ])
    }

    #[test]
    fn round_trip() {
        let data = sample();
        assert_eq!(
            read_resources(&data, b"PiPL").unwrap(),
            vec![(16000, b"one".to_vec()), (16001, b"two".to_vec())]
        );
        assert_eq!(
            read_resources(&data, b"STR ").unwrap(),
            vec![(1, b"x".to_vec())]
        );
        assert!(read_resources(&data, b"NONE").unwrap().is_empty());
        assert!(read_resources(&[0; 8], b"PiPL").is_err());
    }

    #[test]
    fn negative_ids_and_empty_payloads() {
        let data = build_resource_file(&[(*b"PiPL", vec![(-1, Vec::new())])]);
        assert_eq!(
            read_resources(&data, b"PiPL").unwrap(),
            vec![(-1, Vec::new())]
        );
    }

    #[test]
    fn empty_file_has_no_types() {
        let data = build_resource_file(&[]);
        // A type count of 0xFFFF + 1 wraps to zero types.
        assert!(read_resources(&data, b"PiPL").unwrap().is_empty());
    }

    fn put_u32(data: &mut [u8], pos: usize, value: u32) {
        data[pos..pos + 4].copy_from_slice(&value.to_be_bytes());
    }

    #[test]
    fn malformed_maps() {
        let good = sample();
        let map_offset = u32::from_be_bytes(good[4..8].try_into().unwrap()) as usize;
        let type_list = map_offset + MIN_MAP_SIZE;

        let mut data_overrun = good.clone();
        put_u32(&mut data_overrun, 8, u32::MAX);
        let mut map_overrun = good.clone();
        put_u32(&mut map_overrun, 4, u32::MAX - 4);
        let mut small_map = good.clone();
        put_u32(&mut small_map, 12, 27);
        let mut bad_type_list = good.clone();
        bad_type_list[map_offset + TYPE_LIST_OFFSET_FIELD..map_offset + TYPE_LIST_OFFSET_FIELD + 2]
            .copy_from_slice(&u16::MAX.to_be_bytes());
        let mut many_types = good.clone();
        many_types[type_list..type_list + 2].copy_from_slice(&0x7FFFu16.to_be_bytes());
        let mut many_refs = good.clone();
        many_refs[type_list + 6..type_list + 8].copy_from_slice(&u16::MAX.to_be_bytes());
        let mut bad_data_offset = good.clone();
        let reference = type_list + 2 + 2 * TYPE_ENTRY_SIZE;
        put_u32(&mut bad_data_offset, reference + 4, DATA_OFFSET_MASK);

        let cases = [
            ("data overrun", data_overrun),
            ("map overrun", map_overrun),
            ("small map", small_map),
            ("bad type list", bad_type_list),
            ("many types", many_types),
            ("many refs", many_refs),
            ("bad data offset", bad_data_offset),
            ("truncated", good[..good.len() - 1].to_vec()),
        ];
        for (label, data) in cases {
            assert!(
                read_resources(&data, b"PiPL").is_err(),
                "{label} should fail"
            );
        }
    }

    #[test]
    fn overlong_length_is_cut_short() {
        let mut data = sample();
        put_u32(&mut data, BUILT_DATA_OFFSET, 1000);
        let resources = read_resources(&data, b"PiPL").unwrap();
        assert!(resources[0].1.starts_with(b"one"));
        // The cut is at the end of the data section, not the end of the file.
        let data_length = u32::from_be_bytes(data[8..12].try_into().unwrap()) as usize;
        assert_eq!(resources[0].1.len(), data_length - DATA_LENGTH_SIZE);
    }

    #[test]
    fn overlapping_references_are_rejected() {
        let mut data =
            build_resource_file(&[(*b"PiPL", vec![(1, vec![7; 100]), (2, b"x".to_vec())])]);
        let map_offset = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize;
        let second_reference = map_offset + MIN_MAP_SIZE + 2 + TYPE_ENTRY_SIZE + REFERENCE_SIZE;
        // Point the second resource at the first one's 100 bytes.
        put_u32(&mut data, second_reference + 4, 0);
        let error = read_resources(&data, b"PiPL").unwrap_err();
        assert!(error.0.contains("overlap"), "{error}");
    }
}

//! The list of installed filters, with an on-disk scan cache and lookup.
//!
//! Parsing PiPLs is cheap but walking large plug-in folders is not, so scan
//! results are cached per file and reused while the file's size and
//! modification time are unchanged.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::host::discovery::{self, PluginInfo};

/// Bump when the cache format or the meaning of cached entries changes.
const CACHE_VERSION: u32 = 1;

/// Hex digits in a filter id: short enough to type, long enough not to collide.
const ID_LENGTH: usize = 12;

/// Returns the short, stable id of a filter (derived from its key).
///
/// # Examples
///
/// ```
/// let id = eightbf_host::catalog::filter_id("/plugins/Sharpen.8bf|16000|Sharpen");
/// assert_eq!(id.len(), 12);
/// ```
pub fn filter_id(key: &str) -> String {
    // FNV-1a: tiny, stable across versions and platforms.
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hash = key.bytes().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    });
    format!("{hash:016x}")[..ID_LENGTH].to_string()
}

#[derive(Serialize, Deserialize)]
struct CachedFile {
    signature: Option<(u64, f64)>,
    entries: Vec<PluginInfo>,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    files: BTreeMap<String, CachedFile>,
}

/// A file that could not be read during a scan.
#[derive(Clone, Debug, Serialize)]
pub struct ScanProblem {
    pub path: PathBuf,
    pub message: String,
}

/// The filters found in a set of folders.
#[derive(Default)]
pub struct Catalog {
    pub filters: Vec<PluginInfo>,
    pub problems: Vec<ScanProblem>,
}

fn signature(path: &Path) -> Option<(u64, f64)> {
    let target = if path.is_dir() {
        discovery::bundle_executable(path).unwrap_or_else(|| path.to_path_buf())
    } else {
        path.to_path_buf()
    };
    let metadata = std::fs::metadata(target).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    Some((metadata.len(), modified))
}

fn load_cache(path: Option<&Path>) -> BTreeMap<String, CachedFile> {
    let Some(path) = path else {
        return BTreeMap::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    match serde_json::from_str::<CacheFile>(&text) {
        Ok(cache) if cache.version == CACHE_VERSION => cache.files,
        _ => BTreeMap::new(),
    }
}

/// True when every cached entry is for the file it is cached under.
///
/// The cache file is outside the plug-in folders and may have been edited;
/// an entry naming some other binary would let a run load a file that was
/// never scanned. Such entries are ignored and the file is read again.
fn entries_describe(key: &str, entries: &[PluginInfo]) -> bool {
    entries.iter().all(|entry| {
        entry.path == key && (entry.binary.is_empty() || Path::new(&entry.binary).starts_with(key))
    })
}

fn save_cache(path: &Path, files: BTreeMap<String, CachedFile>) -> std::io::Result<()> {
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)?;
    }
    let payload = CacheFile {
        version: CACHE_VERSION,
        files,
    };
    let text = serde_json::to_string(&payload).map_err(std::io::Error::other)?;
    // A uniquely named file in the same folder, so concurrent scans never
    // write into each other's copy before the atomic rename.
    let directory = path.parent().filter(|dir| !dir.as_os_str().is_empty());
    let mut temporary = tempfile::NamedTempFile::new_in(directory.unwrap_or(Path::new(".")))?;
    std::io::Write::write_all(&mut temporary, text.as_bytes())?;
    temporary
        .persist(path)
        .map(|_| ())
        .map_err(|error| error.error)
}

impl Catalog {
    /// Scans `folders`, reusing cached results for unchanged files.
    ///
    /// `cache` is the cache file to read and update; `None` disables caching.
    /// Unreadable files are listed in `problems` rather than failing the scan.
    pub fn scan(folders: &[PathBuf], cache: Option<&Path>) -> Self {
        let cached = load_cache(cache);
        let mut files = BTreeMap::new();
        let mut filters = Vec::new();
        let mut problems = Vec::new();
        for path in discovery::iter_candidates(folders) {
            let key = path.to_string_lossy().into_owned();
            let signature = signature(&path);
            let entries = match cached.get(&key) {
                Some(entry)
                    if entry.signature == signature
                        && signature.is_some()
                        && entries_describe(&key, &entry.entries) =>
                {
                    entry.entries.clone()
                }
                _ => match discovery::read_plugin_path(&path) {
                    Ok(entries) => entries,
                    Err(error) => {
                        problems.push(ScanProblem {
                            path,
                            message: error.to_string(),
                        });
                        continue;
                    }
                },
            };
            filters.extend(
                entries
                    .iter()
                    .filter(|entry| entry.pipl.is_filter() && !entry.pipl.is_hidden())
                    .cloned(),
            );
            files.insert(key, CachedFile { signature, entries });
        }
        filters.sort_by_key(|entry| (entry.category().to_lowercase(), entry.name().to_lowercase()));
        if let Some(cache) = cache {
            if let Err(error) = save_cache(cache, files) {
                problems.push(ScanProblem {
                    path: cache.to_path_buf(),
                    message: format!("could not write the scan cache: {error}"),
                });
            }
        }
        Self { filters, problems }
    }

    /// Finds one filter by id, `Category/Name`, name, or plug-in file path.
    ///
    /// Names match case-insensitively.
    ///
    /// # Errors
    ///
    /// Fails when nothing matches or the query matches several filters;
    /// the message lists close matches or the candidates.
    pub fn resolve(&self, query: &str) -> Result<PluginInfo> {
        let path = Path::new(query);
        if path.exists() {
            return resolve_path(path);
        }
        self.resolve_installed(query)
    }

    /// Like [`Catalog::resolve`], but only among the scanned filters: a
    /// file path is never loaded. Use this for queries from the network.
    ///
    /// # Errors
    ///
    /// Fails when nothing matches or the query matches several filters.
    pub fn resolve_installed(&self, query: &str) -> Result<PluginInfo> {
        let needle = query.to_lowercase();
        if let Some(found) = self.filters.iter().find(|f| filter_id(&f.key()) == needle) {
            return Ok(found.clone());
        }
        let qualified: Vec<&PluginInfo> = self
            .filters
            .iter()
            .filter(|f| format!("{}/{}", f.category(), f.name()).to_lowercase() == needle)
            .collect();
        let named: Vec<&PluginInfo> = if qualified.is_empty() {
            self.filters
                .iter()
                .filter(|f| f.name().to_lowercase() == needle)
                .collect()
        } else {
            qualified
        };
        match named.as_slice() {
            [single] => Ok((*single).clone()),
            [] => Err(self.not_found(query, &needle)),
            several => Err(Error::FilterNotFound {
                message: format!(
                    "'{query}' matches {} filters; use an id instead:\n{}",
                    several.len(),
                    several
                        .iter()
                        .map(|f| format!(
                            "  {}  {}/{}  ({})",
                            filter_id(&f.key()),
                            f.category(),
                            f.name(),
                            f.path
                        ))
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            }),
        }
    }

    fn not_found(&self, query: &str, needle: &str) -> Error {
        let close: Vec<String> = self
            .filters
            .iter()
            .filter(|f| f.name().to_lowercase().contains(needle))
            .take(5)
            .map(|f| format!("  {}/{}", f.category(), f.name()))
            .collect();
        let hint = if self.filters.is_empty() {
            "No filters were found. Add a folder with --plugin-dir, EIGHTBF_HOST_PLUGIN_PATH \
             or plugin_dirs in the config file."
                .to_string()
        } else if close.is_empty() {
            "Run '8bf-host list' to see the installed filters.".to_string()
        } else {
            format!("Did you mean:\n{}", close.join("\n"))
        };
        Error::FilterNotFound {
            message: format!("no filter named '{query}'. {hint}"),
        }
    }
}

/// Resolves a path to a plug-in file holding exactly one visible filter.
fn resolve_path(path: &Path) -> Result<PluginInfo> {
    let entries = discovery::read_plugin_path(path).map_err(|error| Error::FilterNotFound {
        message: format!("{} is not a usable filter: {error}", path.display()),
    })?;
    let filters: Vec<PluginInfo> = entries
        .into_iter()
        .filter(|entry| entry.pipl.is_filter())
        .collect();
    match filters.as_slice() {
        [single] => Ok(single.clone()),
        [] => Err(Error::FilterNotFound {
            message: format!("{} contains no filters", path.display()),
        }),
        several => Err(Error::FilterNotFound {
            message: format!(
                "{} contains {} filters; add its folder with --plugin-dir and name one of: {}",
                path.display(),
                several.len(),
                several
                    .iter()
                    .map(|f| format!("'{}/{}'", f.category(), f.name()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::constants as c;
    use crate::host::discovery::PluginFormat;
    use crate::host::pipl::PiPL;

    fn info(name: &str, category: &str, path: &str) -> PluginInfo {
        let pipl = PiPL {
            kind: c::FILTER_KIND,
            name: name.into(),
            category: category.into(),
            entry_points: [("native".to_string(), "Main".to_string())].into(),
            ..PiPL::default()
        };
        PluginInfo {
            path: path.into(),
            binary: path.into(),
            format: PluginFormat::Native,
            archs: vec!["native".into()],
            pipl,
            file_size: 0,
            file_mtime: 0.0,
            error: None,
            extra: BTreeMap::new(),
        }
    }

    fn catalog() -> Catalog {
        Catalog {
            filters: vec![
                info("Sharpen", "Acme", "/p/a.so"),
                info("Blur", "Acme", "/p/b.so"),
                info("Blur", "Other", "/p/c.so"),
            ],
            problems: Vec::new(),
        }
    }

    #[test]
    fn ids_are_stable_and_short() {
        assert_eq!(filter_id("x"), filter_id("x"));
        assert_ne!(filter_id("x"), filter_id("y"));
        assert_eq!(filter_id("x").len(), ID_LENGTH);
    }

    #[test]
    fn resolves_by_name_qualified_name_and_id() {
        let catalog = catalog();
        assert_eq!(catalog.resolve("sharpen").unwrap().path, "/p/a.so");
        assert_eq!(catalog.resolve("Other/Blur").unwrap().path, "/p/c.so");
        let id = filter_id(&catalog.filters[1].key());
        assert_eq!(catalog.resolve(&id).unwrap().path, "/p/b.so");
    }

    #[test]
    fn ambiguous_names_list_the_candidates() {
        let error = catalog().resolve("Blur").unwrap_err();
        assert_eq!(error.exit_code(), crate::error::exit::FILTER_NOT_FOUND);
        let message = error.to_string();
        assert!(message.contains("Acme/Blur") && message.contains("Other/Blur"));
    }

    #[test]
    fn unknown_names_suggest_close_matches() {
        let message = catalog().resolve("Shar").unwrap_err().to_string();
        assert!(message.contains("Did you mean"));
        let message = Catalog::default().resolve("x").unwrap_err().to_string();
        assert!(message.contains("--plugin-dir"));
    }

    #[test]
    fn installed_lookups_never_load_paths() {
        let error = catalog().resolve_installed("/").unwrap_err();
        assert!(error.to_string().contains("no filter named"));
    }

    #[test]
    fn scanning_a_missing_folder_finds_nothing() {
        let catalog = Catalog::scan(&[PathBuf::from("/nonexistent/8bf-host-test")], None);
        assert!(catalog.filters.is_empty());
    }

    #[test]
    fn cached_entries_must_name_their_own_file() {
        let own = info("Sharpen", "Acme", "/p/a.so");
        assert!(entries_describe("/p/a.so", std::slice::from_ref(&own)));
        let mut bundle = info("Glow", "Acme", "/p/Glow.plugin");
        bundle.binary = "/p/Glow.plugin/Contents/MacOS/Glow".into();
        assert!(entries_describe("/p/Glow.plugin", &[bundle]));
        let mut elsewhere = own.clone();
        elsewhere.binary = "/tmp/evil.so".into();
        assert!(!entries_describe("/p/a.so", &[elsewhere]));
        assert!(!entries_describe("/p/b.so", &[own]));
    }
}

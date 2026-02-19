//! Persistent frequency bookmarks stored in TOML.
//!
//! Bookmarks save favorite frequencies with optional demod mode and decoder,
//! enabling quick recall via `waverunner listen "name"` or TUI hotkeys.

use serde::{Deserialize, Serialize};

/// A single frequency bookmark.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub frequency_hz: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decoder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// Wrapper for TOML serialization.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct BookmarkFile {
    #[serde(default)]
    bookmark: Vec<Bookmark>,
}

/// Persistent bookmark store backed by `~/.config/waverunner/bookmarks.toml`.
#[derive(Debug)]
pub struct BookmarkStore {
    bookmarks: Vec<Bookmark>,
}

impl BookmarkStore {
    /// Load bookmarks from disk, or return empty store if file doesn't exist.
    pub fn load() -> Self {
        let bookmarks = if let Some(path) = bookmark_path() {
            if path.exists() {
                match std::fs::read_to_string(&path) {
                    Ok(content) => match toml::from_str::<BookmarkFile>(&content) {
                        Ok(file) => file.bookmark,
                        Err(e) => {
                            tracing::warn!("Failed to parse bookmarks: {e}");
                            Vec::new()
                        }
                    },
                    Err(e) => {
                        tracing::warn!("Failed to read bookmarks: {e}");
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        Self { bookmarks }
    }

    /// Save bookmarks to disk.
    pub fn save(&self) -> Result<(), String> {
        let path = bookmark_path().ok_or("Cannot determine config directory")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config dir: {e}"))?;
        }
        let file = BookmarkFile {
            bookmark: self.bookmarks.clone(),
        };
        let content = toml::to_string_pretty(&file)
            .map_err(|e| format!("Failed to serialize bookmarks: {e}"))?;
        std::fs::write(&path, content).map_err(|e| format!("Failed to write bookmarks: {e}"))
    }

    /// Add a bookmark. Replaces if name already exists (case-insensitive).
    pub fn add(&mut self, bookmark: Bookmark) {
        self.remove(&bookmark.name);
        self.bookmarks.push(bookmark);
    }

    /// Remove a bookmark by name (case-insensitive).
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.bookmarks.len();
        self.bookmarks
            .retain(|b| !b.name.eq_ignore_ascii_case(name));
        self.bookmarks.len() < before
    }

    /// Find a bookmark by name (case-insensitive).
    pub fn find(&self, name: &str) -> Option<&Bookmark> {
        self.bookmarks
            .iter()
            .find(|b| b.name.eq_ignore_ascii_case(name))
    }

    /// List all bookmarks.
    pub fn list(&self) -> &[Bookmark] {
        &self.bookmarks
    }

    /// Check if the store is empty.
    pub fn is_empty(&self) -> bool {
        self.bookmarks.is_empty()
    }
}

fn bookmark_path() -> Option<std::path::PathBuf> {
    crate::util::config_dir().map(|d| d.join("bookmarks.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_find() {
        let mut store = BookmarkStore {
            bookmarks: Vec::new(),
        };
        store.add(Bookmark {
            name: "Local FM".to_string(),
            frequency_hz: 98_300_000.0,
            mode: Some("wfm".to_string()),
            decoder: None,
            notes: None,
        });
        assert_eq!(store.list().len(), 1);
        let bm = store.find("local fm").unwrap();
        assert_eq!(bm.frequency_hz, 98_300_000.0);
        assert_eq!(bm.mode.as_deref(), Some("wfm"));
    }

    #[test]
    fn case_insensitive_find() {
        let mut store = BookmarkStore {
            bookmarks: Vec::new(),
        };
        store.add(Bookmark {
            name: "ADS-B".to_string(),
            frequency_hz: 1_090_000_000.0,
            mode: None,
            decoder: Some("adsb".to_string()),
            notes: None,
        });
        assert!(store.find("ads-b").is_some());
        assert!(store.find("ADS-B").is_some());
        assert!(store.find("Ads-B").is_some());
    }

    #[test]
    fn remove_bookmark() {
        let mut store = BookmarkStore {
            bookmarks: Vec::new(),
        };
        store.add(Bookmark {
            name: "Test".to_string(),
            frequency_hz: 100e6,
            mode: None,
            decoder: None,
            notes: None,
        });
        assert!(!store.is_empty());
        assert!(store.remove("test"));
        assert!(store.is_empty());
        assert!(!store.remove("nonexistent"));
    }

    #[test]
    fn add_replaces_existing() {
        let mut store = BookmarkStore {
            bookmarks: Vec::new(),
        };
        store.add(Bookmark {
            name: "FM".to_string(),
            frequency_hz: 98_300_000.0,
            mode: None,
            decoder: None,
            notes: None,
        });
        store.add(Bookmark {
            name: "FM".to_string(),
            frequency_hz: 101_100_000.0,
            mode: Some("wfm".to_string()),
            decoder: None,
            notes: None,
        });
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.find("FM").unwrap().frequency_hz, 101_100_000.0);
    }

    #[test]
    fn toml_roundtrip() {
        let file = BookmarkFile {
            bookmark: vec![
                Bookmark {
                    name: "Local FM".to_string(),
                    frequency_hz: 98_300_000.0,
                    mode: Some("wfm".to_string()),
                    decoder: Some("rds".to_string()),
                    notes: Some("My favorite station".to_string()),
                },
                Bookmark {
                    name: "ADS-B".to_string(),
                    frequency_hz: 1_090_000_000.0,
                    mode: None,
                    decoder: Some("adsb".to_string()),
                    notes: None,
                },
            ],
        };
        let toml_str = toml::to_string_pretty(&file).unwrap();
        let parsed: BookmarkFile = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed.bookmark.len(), 2);
        assert_eq!(parsed.bookmark[0].name, "Local FM");
        assert_eq!(parsed.bookmark[1].frequency_hz, 1_090_000_000.0);
    }

    #[test]
    fn empty_store() {
        let store = BookmarkStore {
            bookmarks: Vec::new(),
        };
        assert!(store.is_empty());
        assert!(store.find("anything").is_none());
        assert!(store.list().is_empty());
    }
}

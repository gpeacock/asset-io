//! Test utilities for working with fixture files.
//!
//! This module provides helpers for working with test fixtures, including:
//! - Embedded fixtures (when `embed-fixtures` feature is enabled)
//! - File-based fixtures from `tests/fixtures/`
//! - Extended fixtures from custom directories (via `JUMBF_TEST_FIXTURES` env var)
//!
//! # Usage
//!
//! ```no_run
//! use jumbf_io::test_utils::*;
//!
//! # fn example() -> jumbf_io::Result<()> {
//! // Use predefined fixture constants
//! let path = fixture_path(FIREFLY_TRAIN);
//!
//! // Or get test streams (embedded or file-based)
//! let (format, input, output) = create_test_streams(DESIGNER)?;
//!
//! // List all available fixtures
//! let all_fixtures = list_fixtures()?;
//! # Ok(())
//! # }
//! ```

use std::{
    collections::HashMap,
    fs,
    io::Cursor,
    path::PathBuf,
    sync::LazyLock,
};

use crate::{Error, Result};

/// Type alias for test stream tuples: (format, input_cursor, output_cursor)
pub type TestStreams = (&'static str, Cursor<Vec<u8>>, Cursor<Vec<u8>>);

/// Macro to define fixtures with embedded data and file fallback
macro_rules! define_fixtures {
    ($($name:ident => ($file:expr, $format:expr)),* $(,)?) => {
        // Define constants for fixture names
        $(
            #[allow(dead_code)]
            pub const $name: &str = $file;
        )*

        // Create embedded registry (small files compiled into binary)
        static EMBEDDED_FIXTURES: LazyLock<HashMap<&'static str, (&'static [u8], &'static str)>> = 
            LazyLock::new(|| {
                #[allow(unused_mut)]
                let mut map = HashMap::new();
                $(
                    // Only embed if feature is enabled
                    #[cfg(feature = "embed-fixtures")]
                    {
                        let bytes: &'static [u8] = include_bytes!(concat!("../tests/fixtures/", $file));
                        map.insert($file, (bytes, $format));
                    }
                )*
                map
            });

        /// Get the embedded fixtures registry
        pub fn get_registry() -> &'static HashMap<&'static str, (&'static [u8], &'static str)> {
            &EMBEDDED_FIXTURES
        }
        
        /// List all defined fixtures
        pub fn list_all_fixtures() -> Vec<&'static str> {
            vec![$($file),*]
        }
    };
}

// Define your fixtures
// Minimal committed set for open source distribution
define_fixtures!(
    // Core test fixtures (all <1MB, safe for embedding)
    DESIGNER => ("Designer.jpeg", "image/jpeg"),           // 127KB - JUMBF only
    FIREFLY_TRAIN => ("FireflyTrain.jpg", "image/jpeg"),  // 161KB - XMP + JUMBF
    P1000708 => ("P1000708.jpg", "image/jpeg"),            // 810KB - XMP only
);

/// Get path to a fixture file
/// 
/// Search order:
/// 1. JUMBF_TEST_FIXTURES env var (for extended test sets)
/// 2. Default tests/fixtures directory
pub fn fixture_path(file_name: &str) -> PathBuf {
    // Check JUMBF_TEST_FIXTURES env var first
    if let Ok(custom_dir) = std::env::var("JUMBF_TEST_FIXTURES") {
        let path = PathBuf::from(custom_dir).join(file_name);
        if path.exists() {
            return path;
        }
    }
    
    // Default to tests/fixtures (relative to project root)
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests/fixtures");
    path.push(file_name);
    path
}

/// Create test streams - embedded or file-based
/// 
/// Returns: (format, input_cursor, output_cursor)
/// 
/// If the fixture is embedded, returns a cursor with embedded data.
/// Otherwise, reads from the filesystem.
pub fn create_test_streams(fixture_name: &str) -> Result<TestStreams> {
    // Try embedded fixture first
    if let Some(fixture) = get_registry().get(fixture_name) {
        let data = fixture.0;
        let format = fixture.1;
        
        let input_cursor = Cursor::new(data.to_vec());
        let output_cursor = Cursor::new(Vec::new());
        
        return Ok((format, input_cursor, output_cursor));
    }
    
    // Fallback to file system
    let input_path = fixture_path(fixture_name);
    let input_data = fs::read(&input_path)
        .map_err(Error::Io)?;
    
    let format = input_path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| match ext.to_lowercase().as_str() {
            "jpg" | "jpeg" => "image/jpeg",
            _ => "application/octet-stream",
        })
        .unwrap_or("application/octet-stream");
    
    let input_cursor = Cursor::new(input_data);
    let output_cursor = Cursor::new(Vec::new());
    
    Ok((format, input_cursor, output_cursor))
}

/// Helper to get fixture data as bytes
pub fn fixture_bytes(name: &str) -> Result<Vec<u8>> {
    // Try embedded first
    if let Some(fixture) = get_registry().get(name) {
        return Ok(fixture.0.to_vec());
    }
    
    // Read from file
    fs::read(fixture_path(name))
        .map_err(Error::Io)
}

/// List all available fixtures (from embedded + filesystem)
/// 
/// This will:
/// 1. List all defined fixtures
/// 2. If JUMBF_TEST_FIXTURES is set, also list files from that directory
pub fn list_fixtures() -> Result<Vec<String>> {
    let mut fixtures = Vec::new();
    
    // Add all defined fixtures
    for name in list_all_fixtures() {
        fixtures.push(name.to_string());
    }
    
    // Check for extended fixtures directory
    if let Ok(custom_dir) = std::env::var("JUMBF_TEST_FIXTURES") {
        let extended_path = PathBuf::from(custom_dir);
        if extended_path.exists() && extended_path.is_dir() {
            for entry in fs::read_dir(extended_path).map_err(Error::Io)? {
                let entry = entry.map_err(Error::Io)?;
                let path = entry.path();
                if path.is_file() {
                    if let Some(ext) = path.extension() {
                        if ext == "jpg" || ext == "jpeg" || ext == "JPG" || ext == "JPEG" {
                            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                                // Only add if not already in defined fixtures
                                if !fixtures.contains(&name.to_string()) {
                                    fixtures.push(name.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    
    Ok(fixtures)
}

/// Check if a fixture is embedded
pub fn is_embedded(fixture_name: &str) -> bool {
    get_registry().contains_key(fixture_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fixture_constants() {
        assert_eq!(FIREFLY_TRAIN, "FireflyTrain.jpg");
        assert_eq!(DESIGNER, "Designer.jpeg");
        assert_eq!(P1000708, "P1000708.jpg");
    }

    #[test]
    fn test_fixture_path() {
        let path = fixture_path(FIREFLY_TRAIN);
        assert!(path.to_string_lossy().contains("FireflyTrain.jpg"));
    }

    #[test]
    fn test_list_all_fixtures() {
        let fixtures = list_all_fixtures();
        assert!(fixtures.contains(&"FireflyTrain.jpg"));
        assert!(fixtures.contains(&"Designer.jpeg"));
        assert!(fixtures.contains(&"P1000708.jpg"));
        assert_eq!(fixtures.len(), 3);
    }

    #[cfg(feature = "embed-fixtures")]
    #[test]
    fn test_embedded_fixtures() {
        let registry = get_registry();
        assert!(!registry.is_empty());
        // When embed-fixtures is enabled, at least some should be embedded
    }

    #[cfg(not(feature = "embed-fixtures"))]
    #[test]
    fn test_no_embedded_fixtures() {
        let registry = get_registry();
        assert!(registry.is_empty());
    }
}

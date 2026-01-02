//! Minimal XMP parser
//!
//! This module provides a lightweight XMP implementation for extracting and modifying
//! simple key/value pairs in XMP metadata. Use [`MiniXmp`] for the primary API.
//!
//! # Quick Start
//!
//! ```
//! use asset_io::MiniXmp;
//!
//! let xmp_data = r#"<rdf:Description dc:title="Photo" dc:creator="John" />"#;
//! let xmp = MiniXmp::new(xmp_data);
//!
//! // Read values
//! assert_eq!(xmp.get("dc:title"), Some("Photo".to_string()));
//! assert_eq!(xmp.get("dc:creator"), Some("John".to_string()));
//!
//! // Modify and get updated XMP
//! let updated = xmp.set("dc:title", "New Photo").unwrap();
//! let updated = updated.remove("dc:creator").unwrap();
//! let xmp_string = updated.into_string();
//! ```
//!
//! # Batch Operations
//!
//! For efficiency with multiple keys:
//!
//! ```
//! use asset_io::MiniXmp;
//!
//! let xmp = MiniXmp::new(r#"<rdf:Description dc:title="Photo" dc:creator="John" />"#);
//!
//! // Get multiple values in one pass
//! let values = xmp.get_many(&["dc:title", "dc:creator", "dc:format"]);
//! assert_eq!(values[0], Some("Photo".to_string()));
//! assert_eq!(values[2], None);  // Not found
//!
//! // Apply multiple updates at once
//! let updates = [
//!     ("dc:title", Some("New Photo")),
//!     ("dc:subject", Some("Landscape")),
//!     ("dc:creator", None),  // None = remove
//! ];
//! let updated = xmp.apply_updates(&updates).unwrap();
//! ```
//!
//! # Limitations
//!
//! This is a **minimal** XMP implementation ("Mini" is in the name!).
//! It handles the most common XMP patterns but has limitations:
//!
//! ## Not Supported
//!
//! - Structured properties (nested elements, alt/bag/seq containers)
//! - Arrays (only simple string values)
//! - XMP packet padding (writes don't maintain the 2-4KB minimum size)
//! - Namespace validation (assumes caller provides correct prefixes)
//!
//! ## What Works
//!
//! - Simple attribute-based properties (the most common XMP pattern)
//! - Reading child elements with text content
//! - Multiple `rdf:Description` blocks
//! - XML entity encoding/decoding
//! - UTF-8 text values
//!
//! For full XMP support, consider the `xmp-toolkit` crate instead.

use crate::error::Result;
use std::io::Cursor;

const RDF_DESCRIPTION: &[u8] = b"rdf:Description";

// ============================================================================
// MiniXmp Struct API
// ============================================================================

/// A minimal XMP parser and editor
///
/// `MiniXmp` provides a lightweight way to read and modify XMP metadata without
/// pulling in heavy dependencies. It handles the most common XMP patterns
/// (simple attribute-based properties) but is not a full XMP implementation.
///
/// # Example
///
/// ```
/// use asset_io::MiniXmp;
///
/// // Parse XMP from bytes or string
/// let xmp = MiniXmp::new(r#"<rdf:Description dc:title="Photo" />"#);
///
/// // Read a value
/// if let Some(title) = xmp.get("dc:title") {
///     println!("Title: {}", title);
/// }
///
/// // Modify (returns new MiniXmp)
/// let updated = xmp.set("dc:creator", "John Doe").unwrap();
///
/// // Get the updated XMP string
/// let xmp_string = updated.into_string();
/// ```
#[derive(Debug, Clone)]
pub struct MiniXmp {
    data: String,
}

impl MiniXmp {
    /// Create a new MiniXmp from an XMP string
    pub fn new(xmp: impl Into<String>) -> Self {
        Self { data: xmp.into() }
    }

    /// Create a MiniXmp from raw bytes
    ///
    /// Returns `None` if the bytes are not valid UTF-8.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        std::str::from_utf8(bytes).ok().map(|s| Self::new(s))
    }

    /// Get the XMP as a string slice
    pub fn as_str(&self) -> &str {
        &self.data
    }

    /// Get the XMP as bytes
    pub fn as_bytes(&self) -> &[u8] {
        self.data.as_bytes()
    }

    /// Consume self and return the XMP string
    pub fn into_string(self) -> String {
        self.data
    }

    /// Consume self and return the XMP as bytes
    pub fn into_bytes(self) -> Vec<u8> {
        self.data.into_bytes()
    }

    /// Get a single value from the XMP
    ///
    /// Returns `None` if the key is not found.
    pub fn get(&self, key: &str) -> Option<String> {
        get_key(&self.data, key)
    }

    /// Get multiple values from the XMP in a single pass
    ///
    /// More efficient than calling `get()` multiple times.
    pub fn get_many(&self, keys: &[&str]) -> Vec<Option<String>> {
        get_keys(&self.data, keys)
    }

    /// Set a value in the XMP, returning a new MiniXmp
    ///
    /// If the key exists, it will be updated. If not, it will be added.
    pub fn set(&self, key: &str, value: &str) -> Result<Self> {
        add_key(&self.data, key, value).map(Self::new)
    }

    /// Remove a key from the XMP, returning a new MiniXmp
    pub fn remove(&self, key: &str) -> Result<Self> {
        remove_key(&self.data, key).map(Self::new)
    }

    /// Apply multiple updates at once, returning a new MiniXmp
    ///
    /// Each update is a `(key, value)` pair where `value` is:
    /// - `Some("value")` to set/add the key
    /// - `None` to remove the key
    pub fn apply_updates(&self, updates: &[(&str, Option<&str>)]) -> Result<Self> {
        apply_updates(&self.data, updates).map(Self::new)
    }

    /// Check if the XMP contains a key
    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Get the length of the XMP string in bytes
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Check if the XMP is empty
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }
}

impl std::fmt::Display for MiniXmp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.data)
    }
}

impl From<String> for MiniXmp {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for MiniXmp {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

impl AsRef<str> for MiniXmp {
    fn as_ref(&self) -> &str {
        &self.data
    }
}

// ============================================================================
// Free Functions (kept for compatibility, delegate to implementation below)
// ============================================================================

/// Validate that a key is a reasonable XMP property name.
///
/// This is a basic check - it doesn't validate against full XMP spec,
/// just catches obvious errors like empty keys or keys with spaces/quotes.
fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(crate::Error::InvalidFormat(
            "XMP key cannot be empty".to_string(),
        ));
    }

    // Check for characters that would break XML attribute syntax
    if key.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>') {
        return Err(crate::Error::InvalidFormat(format!(
            "Invalid XMP key '{}': contains whitespace or XML special characters",
            key
        )));
    }

    Ok(())
}

/// Get multiple values from XMP in a single pass.
///
/// Returns a Vec with the same length as `keys`, where each position
/// corresponds to the key at that position. Missing keys return `None`.
///
/// This is much more efficient than calling [`get_key()`] multiple times,
/// as it only parses the XMP once.
///
/// # Multiple `rdf:Description` Blocks
///
/// Adobe and other tools often split XMP across multiple `rdf:Description`
/// blocks organized by namespace. This function processes **all blocks**
/// to provide a complete view of the metadata. If a key appears in multiple
/// blocks, the **last occurrence wins**.
///
/// # Example
///
/// ```
/// use asset_io::xmp::get_keys;
///
/// let xmp = r#"<rdf:Description dc:title="Photo" dc:creator="John" />"#;
/// let values = get_keys(xmp, &["dc:title", "dc:creator", "dc:format"]);
///
/// assert_eq!(values[0], Some("Photo".to_string()));
/// assert_eq!(values[1], Some("John".to_string()));
/// assert_eq!(values[2], None);  // Not found
/// ```
pub fn get_keys(xmp: &str, keys: &[&str]) -> Vec<Option<String>> {
    use quick_xml::{events::Event, name::QName, Reader};

    let mut reader = Reader::from_str(xmp);
    reader.config_mut().trim_text(true);

    // Track which keys we've found (allow overwriting from later blocks)
    let mut results = vec![None; keys.len()];

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if e.name() == QName(RDF_DESCRIPTION) {
                    // Search attributes
                    for attr in e.attributes().flatten() {
                        // Check if this attribute matches any of our keys
                        for (i, key) in keys.iter().enumerate() {
                            if attr.key == QName(key.as_bytes()) {
                                // Use decode_and_unescape_value to handle XML entities
                                if let Ok(s) = attr.decode_and_unescape_value(reader.decoder()) {
                                    // Overwrite if found in later block (last wins)
                                    results[i] = Some(s.to_string());
                                }
                            }
                        }
                    }
                } else {
                    // Search as element
                    for (i, key) in keys.iter().enumerate() {
                        if e.name() == QName(key.as_bytes()) {
                            if let Ok(s) = reader.read_text(e.name()) {
                                // Overwrite if found in later block (last wins)
                                results[i] = Some(s.to_string());
                            }
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }

    results
}

/// Apply multiple updates to XMP in a single pass.
///
/// Each update is a tuple of `(key, value)` where:
/// - `Some(value)` = add or replace the key
/// - `None` = remove the key
///
/// This is much more efficient than calling [`add_key()`] or [`remove_key()`]
/// multiple times, as it only parses and rebuilds the XMP once.
///
/// # Multiple `rdf:Description` Blocks
///
/// This function modifies **only the first** `rdf:Description` block.
/// Other blocks are preserved unchanged. This ensures predictable behavior
/// and avoids accidentally duplicating properties across blocks.
///
/// If you need to modify properties in other blocks, consider extracting
/// and rewriting the specific block you need.
///
/// # Example
///
/// ```
/// use asset_io::xmp::apply_updates;
///
/// let xmp = r#"<rdf:Description dc:title="Old" dc:creator="John" />"#;
/// let updates = [
///     ("dc:title", Some("New")),
///     ("dc:subject", Some("Landscape")),
///     ("dc:creator", None),  // Remove
/// ];
/// let updated = apply_updates(xmp, &updates).unwrap();
/// ```
pub fn apply_updates(xmp: &str, updates: &[(&str, Option<&str>)]) -> Result<String> {
    use quick_xml::{
        events::{BytesStart, Event},
        name::QName,
        Reader, Writer,
    };

    // Validate all keys upfront
    for (key, _) in updates {
        validate_key(key)?;
    }

    let mut reader = Reader::from_str(xmp);
    reader.config_mut().trim_text(false);
    reader.config_mut().expand_empty_elements = false;

    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut applied = false;

    loop {
        let event = reader.read_event()?;
        match event {
            Event::Start(ref e) if e.name() == QName(RDF_DESCRIPTION) && !applied => {
                let mut elem = BytesStart::new(std::str::from_utf8(RDF_DESCRIPTION).unwrap());

                // Track which keys we've seen in the original attributes
                let mut seen_keys = std::collections::HashSet::new();

                // Copy existing attributes, applying updates
                for attr_result in e.attributes() {
                    match attr_result {
                        Ok(attr) => {
                            let attr_key_str = std::str::from_utf8(attr.key.as_ref()).ok();

                            // Check if this attribute has an update
                            let mut handled = false;
                            if let Some(key_str) = attr_key_str {
                                seen_keys.insert(key_str.to_string());

                                for (update_key, update_value) in updates {
                                    if key_str == *update_key {
                                        // Apply update: Some = replace, None = remove
                                        if let Some(new_value) = update_value {
                                            elem.push_attribute((*update_key, *new_value));
                                        }
                                        handled = true;
                                        break;
                                    }
                                }
                            }

                            // Keep original if no update
                            if !handled {
                                elem.extend_attributes([attr]);
                            }
                        }
                        Err(e) => return Err(crate::Error::InvalidFormat(e.to_string())),
                    }
                }

                // Add new keys that weren't in the original (O(1) lookup now!)
                for (key, value) in updates {
                    if let Some(v) = value {
                        if !seen_keys.contains(*key) {
                            elem.push_attribute((*key, *v));
                        }
                    }
                }

                applied = true;
                writer.write_event(Event::Start(elem))?;
            }
            Event::Empty(ref e) if e.name() == QName(RDF_DESCRIPTION) && !applied => {
                let mut elem = BytesStart::new(std::str::from_utf8(RDF_DESCRIPTION).unwrap());

                // Track which keys we've seen in the original attributes
                let mut seen_keys = std::collections::HashSet::new();

                // Copy existing attributes, applying updates
                for attr_result in e.attributes() {
                    match attr_result {
                        Ok(attr) => {
                            let attr_key_str = std::str::from_utf8(attr.key.as_ref()).ok();

                            // Check if this attribute has an update
                            let mut handled = false;
                            if let Some(key_str) = attr_key_str {
                                seen_keys.insert(key_str.to_string());

                                for (update_key, update_value) in updates {
                                    if key_str == *update_key {
                                        // Apply update: Some = replace, None = remove
                                        if let Some(new_value) = update_value {
                                            elem.push_attribute((*update_key, *new_value));
                                        }
                                        handled = true;
                                        break;
                                    }
                                }
                            }

                            // Keep original if no update
                            if !handled {
                                elem.extend_attributes([attr]);
                            }
                        }
                        Err(e) => return Err(crate::Error::InvalidFormat(e.to_string())),
                    }
                }

                // Add new keys that weren't in the original (O(1) lookup now!)
                for (key, value) in updates {
                    if let Some(v) = value {
                        if !seen_keys.contains(*key) {
                            elem.push_attribute((*key, *v));
                        }
                    }
                }

                applied = true;
                writer.write_event(Event::Empty(elem))?;
            }
            Event::Eof => break,
            e => writer.write_event(e)?,
        }
    }

    let result = writer.into_inner().into_inner();
    String::from_utf8(result).map_err(|e| crate::Error::InvalidFormat(e.to_string()))
}

/// Get a single value from XMP (convenience wrapper).
///
/// For getting multiple values, use [`get_keys()`] instead to parse only once.
///
/// # Example
///
/// ```
/// use asset_io::xmp::get_key;
///
/// let xmp = r#"<rdf:Description dc:title="My Photo" />"#;
/// assert_eq!(get_key(xmp, "dc:title"), Some("My Photo".to_string()));
/// ```
pub fn get_key(xmp: &str, key: &str) -> Option<String> {
    get_keys(xmp, &[key]).into_iter().next().flatten()
}

/// Add or replace a single key in XMP (convenience wrapper).
///
/// For multiple updates, use [`apply_updates()`] instead to parse only once.
///
/// # Example
///
/// ```
/// use asset_io::xmp::{add_key, get_key};
///
/// let xmp = r#"<?xpacket begin=""?><rdf:RDF><rdf:Description /></rdf:RDF><?xpacket end="w"?>"#;
/// let updated = add_key(xmp, "dc:title", "My Photo").unwrap();
/// assert!(updated.contains(r#"dc:title="My Photo""#));
/// ```
pub fn add_key(xmp: &str, key: &str, value: &str) -> Result<String> {
    apply_updates(xmp, &[(key, Some(value))])
}

/// Remove a single key from XMP (convenience wrapper).
///
/// For multiple updates, use [`apply_updates()`] instead to parse only once.
///
/// # Example
///
/// ```
/// use asset_io::xmp::{remove_key, get_key};
///
/// let xmp = r#"<rdf:Description dc:title="My Photo" dc:creator="John" />"#;
/// let updated = remove_key(xmp, "dc:title").unwrap();
/// assert_eq!(get_key(&updated, "dc:title"), None);
/// assert_eq!(get_key(&updated, "dc:creator"), Some("John".to_string()));
/// ```
pub fn remove_key(xmp: &str, key: &str) -> Result<String> {
    apply_updates(xmp, &[(key, None)])
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_XMP: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
    <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
        <rdf:Description rdf:about=""
            xmlns:dc="http://purl.org/dc/elements/1.1/"
            xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/"
            dc:format="image/jpeg"
            xmpMM:DocumentID="xmp.did:1234">
        </rdf:Description>
    </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;

    #[test]
    fn test_get_key() {
        assert_eq!(
            get_key(TEST_XMP, "dc:format"),
            Some("image/jpeg".to_string())
        );
        assert_eq!(
            get_key(TEST_XMP, "xmpMM:DocumentID"),
            Some("xmp.did:1234".to_string())
        );
        assert_eq!(get_key(TEST_XMP, "nonexistent"), None);
    }

    #[test]
    fn test_add_key() {
        let xmp = add_key(TEST_XMP, "dc:title", "My Photo").unwrap();
        assert_eq!(get_key(&xmp, "dc:title"), Some("My Photo".to_string()));
        // Original keys should still be there
        assert_eq!(get_key(&xmp, "dc:format"), Some("image/jpeg".to_string()));
    }

    #[test]
    fn test_replace_key() {
        let xmp = add_key(TEST_XMP, "dc:format", "image/png").unwrap();
        assert_eq!(get_key(&xmp, "dc:format"), Some("image/png".to_string()));
    }

    #[test]
    fn test_remove_key() {
        let xmp = remove_key(TEST_XMP, "dc:format").unwrap();
        assert_eq!(get_key(&xmp, "dc:format"), None);
        // Other keys should still be there
        assert_eq!(
            get_key(&xmp, "xmpMM:DocumentID"),
            Some("xmp.did:1234".to_string())
        );
    }

    #[test]
    fn test_remove_nonexistent_key() {
        let xmp = remove_key(TEST_XMP, "nonexistent").unwrap();
        // Should not error, just return unchanged XMP
        assert_eq!(get_key(&xmp, "dc:format"), Some("image/jpeg".to_string()));
    }

    #[test]
    fn test_get_keys_batch() {
        let values = get_keys(TEST_XMP, &["dc:format", "xmpMM:DocumentID", "nonexistent"]);
        assert_eq!(values.len(), 3);
        assert_eq!(values[0], Some("image/jpeg".to_string()));
        assert_eq!(values[1], Some("xmp.did:1234".to_string()));
        assert_eq!(values[2], None);
    }

    #[test]
    fn test_get_keys_empty() {
        let values = get_keys(TEST_XMP, &[]);
        assert_eq!(values.len(), 0);
    }

    #[test]
    fn test_apply_updates_add_and_replace() {
        let updates = [
            ("dc:title", Some("My Photo")),
            ("dc:format", Some("image/png")),  // Replace existing
            ("dc:subject", Some("Landscape")), // Add new
        ];
        let xmp = apply_updates(TEST_XMP, &updates).unwrap();

        assert_eq!(get_key(&xmp, "dc:title"), Some("My Photo".to_string()));
        assert_eq!(get_key(&xmp, "dc:format"), Some("image/png".to_string()));
        assert_eq!(get_key(&xmp, "dc:subject"), Some("Landscape".to_string()));
        // Original key should still be there
        assert_eq!(
            get_key(&xmp, "xmpMM:DocumentID"),
            Some("xmp.did:1234".to_string())
        );
    }

    #[test]
    fn test_apply_updates_remove() {
        let updates = [
            ("dc:format", None),       // Remove
            ("dc:title", Some("New")), // Add
        ];
        let xmp = apply_updates(TEST_XMP, &updates).unwrap();

        assert_eq!(get_key(&xmp, "dc:format"), None);
        assert_eq!(get_key(&xmp, "dc:title"), Some("New".to_string()));
        assert_eq!(
            get_key(&xmp, "xmpMM:DocumentID"),
            Some("xmp.did:1234".to_string())
        );
    }

    #[test]
    fn test_apply_updates_mixed() {
        let updates = [
            ("dc:format", Some("image/png")), // Replace
            ("dc:title", Some("Photo")),      // Add
            ("xmpMM:DocumentID", None),       // Remove
            ("dc:creator", Some("John")),     // Add
        ];
        let xmp = apply_updates(TEST_XMP, &updates).unwrap();

        assert_eq!(get_key(&xmp, "dc:format"), Some("image/png".to_string()));
        assert_eq!(get_key(&xmp, "dc:title"), Some("Photo".to_string()));
        assert_eq!(get_key(&xmp, "xmpMM:DocumentID"), None);
        assert_eq!(get_key(&xmp, "dc:creator"), Some("John".to_string()));
    }

    #[test]
    fn test_batch_vs_single_consistency() {
        // Batch operation
        let updates = [("dc:title", Some("Test"))];
        let batch_result = apply_updates(TEST_XMP, &updates).unwrap();

        // Single operation
        let single_result = add_key(TEST_XMP, "dc:title", "Test").unwrap();

        // Both should produce the same result
        assert_eq!(
            get_key(&batch_result, "dc:title"),
            get_key(&single_result, "dc:title")
        );
    }

    #[test]
    fn test_xml_entity_escaping() {
        // Test that special XML characters are properly escaped and unescaped
        let test_cases = [
            ("&", "ampersand"),
            ("<", "less than"),
            (">", "greater than"),
            ("\"", "double quote"),
            ("'", "single quote"),
            ("Photo & Video", "mixed ampersand"),
            ("<tag>", "tags"),
            ("Quote: \"test\"", "quoted text"),
        ];

        let xmp = r#"<rdf:Description />"#;

        for (input, description) in test_cases {
            let xmp_with_value = add_key(xmp, "dc:title", input).unwrap();
            let value = get_key(&xmp_with_value, "dc:title");
            // Should round-trip correctly
            assert_eq!(
                value,
                Some(input.to_string()),
                "XML entity round-trip failed for {}: input={:?}",
                description,
                input
            );
        }
    }

    #[test]
    fn test_key_validation() {
        let xmp = r#"<rdf:Description dc:title="Photo" />"#;

        // These should fail
        assert!(add_key(xmp, "", "value").is_err(), "Empty key should fail");
        assert!(
            add_key(xmp, "dc:title with space", "value").is_err(),
            "Key with space should fail"
        );
        assert!(
            add_key(xmp, "dc:title\"quote", "value").is_err(),
            "Key with quote should fail"
        );
        assert!(
            add_key(xmp, "dc:title<tag", "value").is_err(),
            "Key with < should fail"
        );
        assert!(
            add_key(xmp, "dc:title>tag", "value").is_err(),
            "Key with > should fail"
        );

        // These should succeed
        assert!(
            add_key(xmp, "dc:title", "value").is_ok(),
            "Normal key should work"
        );
        assert!(
            add_key(xmp, "dc:subject", "value").is_ok(),
            "Another normal key should work"
        );
        assert!(
            add_key(xmp, "my_custom:field", "value").is_ok(),
            "Underscore key should work"
        );
    }

    #[test]
    fn test_multiple_rdf_description_blocks() {
        // XMP with multiple rdf:Description blocks (common in Adobe files)
        let xmp = r#"<?xml version="1.0"?>
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
         xmlns:dc="http://purl.org/dc/elements/1.1/"
         xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/">
  <rdf:Description rdf:about=""
                   dc:title="First Block Title"
                   dc:creator="Alice">
  </rdf:Description>
  <rdf:Description rdf:about=""
                   photoshop:DateCreated="2024-01-15"
                   dc:title="Second Block Title">
  </rdf:Description>
  <rdf:Description rdf:about=""
                   dc:subject="Landscape">
  </rdf:Description>
</rdf:RDF>"#;

        // Test that we can read from all blocks
        let values = get_keys(
            xmp,
            &[
                "dc:title",
                "dc:creator",
                "photoshop:DateCreated",
                "dc:subject",
            ],
        );

        // dc:title appears in both blocks - last one should win
        assert_eq!(
            values[0],
            Some("Second Block Title".to_string()),
            "Should get title from second block (last wins)"
        );
        assert_eq!(
            values[1],
            Some("Alice".to_string()),
            "Should get creator from first block"
        );
        assert_eq!(
            values[2],
            Some("2024-01-15".to_string()),
            "Should get date from second block"
        );
        assert_eq!(
            values[3],
            Some("Landscape".to_string()),
            "Should get subject from third block"
        );

        // Test single-key access too
        assert_eq!(
            get_key(xmp, "dc:title"),
            Some("Second Block Title".to_string())
        );
        assert_eq!(get_key(xmp, "dc:creator"), Some("Alice".to_string()));
        assert_eq!(
            get_key(xmp, "photoshop:DateCreated"),
            Some("2024-01-15".to_string())
        );
    }

    #[test]
    fn test_write_only_modifies_first_block() {
        // XMP with multiple blocks
        let xmp = r#"<rdf:RDF>
  <rdf:Description dc:title="First" dc:creator="Alice" />
  <rdf:Description photoshop:DateCreated="2024-01-15" />
</rdf:RDF>"#;

        // Modify a key
        let updated = apply_updates(xmp, &[("dc:title", Some("Modified"))]).unwrap();

        // First block should be modified
        assert!(
            updated.contains("dc:title=\"Modified\""),
            "First block should be updated"
        );

        // Second block should be unchanged
        assert!(
            updated.contains("photoshop:DateCreated=\"2024-01-15\""),
            "Second block should be preserved"
        );

        // Verify we can still read from both blocks
        let values = get_keys(&updated, &["dc:title", "photoshop:DateCreated"]);
        assert_eq!(values[0], Some("Modified".to_string()));
        assert_eq!(values[1], Some("2024-01-15".to_string()));
    }
}

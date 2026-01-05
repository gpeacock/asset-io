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
        std::str::from_utf8(bytes).ok().map(Self::new)
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

    /// Alias for `into_string` - consume self and return the inner string
    pub fn into_inner(self) -> String {
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
        get_keys_impl(&self.data, &[key]).into_iter().next().flatten()
    }

    /// Get multiple values from the XMP in a single pass
    ///
    /// More efficient than calling `get()` multiple times.
    pub fn get_many(&self, keys: &[&str]) -> Vec<Option<String>> {
        get_keys_impl(&self.data, keys)
    }

    /// Set a value in the XMP, returning a new MiniXmp
    ///
    /// If the key exists, it will be updated. If not, it will be added.
    pub fn set(&self, key: &str, value: &str) -> Result<Self> {
        apply_updates_impl(&self.data, &[(key, Some(value))]).map(Self::new)
    }

    /// Remove a key from the XMP, returning a new MiniXmp
    pub fn remove(&self, key: &str) -> Result<Self> {
        apply_updates_impl(&self.data, &[(key, None)]).map(Self::new)
    }

    /// Apply multiple updates at once, returning a new MiniXmp
    ///
    /// Each update is a `(key, value)` pair where `value` is:
    /// - `Some("value")` to set/add the key
    /// - `None` to remove the key
    pub fn apply_updates(&self, updates: &[(&str, Option<&str>)]) -> Result<Self> {
        apply_updates_impl(&self.data, updates).map(Self::new)
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
// Internal Implementation
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

/// Get multiple values from XMP in a single pass (internal implementation).
fn get_keys_impl(xmp: &str, keys: &[&str]) -> Vec<Option<String>> {
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

/// Apply multiple updates to XMP in a single pass (internal implementation).
fn apply_updates_impl(xmp: &str, updates: &[(&str, Option<&str>)]) -> Result<String> {
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
                let elem_name = std::str::from_utf8(RDF_DESCRIPTION)
                    .map_err(|_| crate::Error::InvalidFormat("Invalid RDF_DESCRIPTION constant".into()))?;
                let mut elem = BytesStart::new(elem_name);

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
                let elem_name = std::str::from_utf8(RDF_DESCRIPTION)
                    .map_err(|_| crate::Error::InvalidFormat("Invalid RDF_DESCRIPTION constant".into()))?;
                let mut elem = BytesStart::new(elem_name);

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
        let xmp = MiniXmp::new(TEST_XMP);
        assert_eq!(xmp.get("dc:format"), Some("image/jpeg".to_string()));
        assert_eq!(xmp.get("xmpMM:DocumentID"), Some("xmp.did:1234".to_string()));
        assert_eq!(xmp.get("nonexistent"), None);
    }

    #[test]
    fn test_add_key() {
        let xmp = MiniXmp::new(TEST_XMP);
        let updated = xmp.set("dc:title", "My Photo").unwrap();
        assert_eq!(updated.get("dc:title"), Some("My Photo".to_string()));
        // Original keys should still be there
        assert_eq!(updated.get("dc:format"), Some("image/jpeg".to_string()));
    }

    #[test]
    fn test_replace_key() {
        let xmp = MiniXmp::new(TEST_XMP);
        let updated = xmp.set("dc:format", "image/png").unwrap();
        assert_eq!(updated.get("dc:format"), Some("image/png".to_string()));
    }

    #[test]
    fn test_remove_key() {
        let xmp = MiniXmp::new(TEST_XMP);
        let updated = xmp.remove("dc:format").unwrap();
        assert_eq!(updated.get("dc:format"), None);
        // Other keys should still be there
        assert_eq!(
            updated.get("xmpMM:DocumentID"),
            Some("xmp.did:1234".to_string())
        );
    }

    #[test]
    fn test_remove_nonexistent_key() {
        let xmp = MiniXmp::new(TEST_XMP);
        let updated = xmp.remove("nonexistent").unwrap();
        // Should not error, just return unchanged XMP
        assert_eq!(updated.get("dc:format"), Some("image/jpeg".to_string()));
    }

    #[test]
    fn test_get_keys_batch() {
        let xmp = MiniXmp::new(TEST_XMP);
        let values = xmp.get_many(&["dc:format", "xmpMM:DocumentID", "nonexistent"]);
        assert_eq!(values.len(), 3);
        assert_eq!(values[0], Some("image/jpeg".to_string()));
        assert_eq!(values[1], Some("xmp.did:1234".to_string()));
        assert_eq!(values[2], None);
    }

    #[test]
    fn test_get_keys_empty() {
        let xmp = MiniXmp::new(TEST_XMP);
        let values = xmp.get_many(&[]);
        assert_eq!(values.len(), 0);
    }

    #[test]
    fn test_apply_updates_add_and_replace() {
        let xmp = MiniXmp::new(TEST_XMP);
        let updates = [
            ("dc:title", Some("My Photo")),
            ("dc:format", Some("image/png")),  // Replace existing
            ("dc:subject", Some("Landscape")), // Add new
        ];
        let updated = xmp.apply_updates(&updates).unwrap();

        assert_eq!(updated.get("dc:title"), Some("My Photo".to_string()));
        assert_eq!(updated.get("dc:format"), Some("image/png".to_string()));
        assert_eq!(updated.get("dc:subject"), Some("Landscape".to_string()));
        // Original key should still be there
        assert_eq!(
            updated.get("xmpMM:DocumentID"),
            Some("xmp.did:1234".to_string())
        );
    }

    #[test]
    fn test_apply_updates_remove() {
        let xmp = MiniXmp::new(TEST_XMP);
        let updates = [
            ("dc:format", None),       // Remove
            ("dc:title", Some("New")), // Add
        ];
        let updated = xmp.apply_updates(&updates).unwrap();

        assert_eq!(updated.get("dc:format"), None);
        assert_eq!(updated.get("dc:title"), Some("New".to_string()));
        assert_eq!(
            updated.get("xmpMM:DocumentID"),
            Some("xmp.did:1234".to_string())
        );
    }

    #[test]
    fn test_apply_updates_mixed() {
        let xmp = MiniXmp::new(TEST_XMP);
        let updates = [
            ("dc:format", Some("image/png")), // Replace
            ("dc:title", Some("Photo")),      // Add
            ("xmpMM:DocumentID", None),       // Remove
            ("dc:creator", Some("John")),     // Add
        ];
        let updated = xmp.apply_updates(&updates).unwrap();

        assert_eq!(updated.get("dc:format"), Some("image/png".to_string()));
        assert_eq!(updated.get("dc:title"), Some("Photo".to_string()));
        assert_eq!(updated.get("xmpMM:DocumentID"), None);
        assert_eq!(updated.get("dc:creator"), Some("John".to_string()));
    }

    #[test]
    fn test_batch_vs_single_consistency() {
        let xmp = MiniXmp::new(TEST_XMP);

        // Batch operation
        let batch_result = xmp.apply_updates(&[("dc:title", Some("Test"))]).unwrap();

        // Single operation
        let single_result = xmp.set("dc:title", "Test").unwrap();

        // Both should produce the same result
        assert_eq!(batch_result.get("dc:title"), single_result.get("dc:title"));
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

        let xmp = MiniXmp::new(r#"<rdf:Description />"#);

        for (input, description) in test_cases {
            let updated = xmp.set("dc:title", input).unwrap();
            let value = updated.get("dc:title");
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
        let xmp = MiniXmp::new(r#"<rdf:Description dc:title="Photo" />"#);

        // These should fail
        assert!(xmp.set("", "value").is_err(), "Empty key should fail");
        assert!(
            xmp.set("dc:title with space", "value").is_err(),
            "Key with space should fail"
        );
        assert!(
            xmp.set("dc:title\"quote", "value").is_err(),
            "Key with quote should fail"
        );
        assert!(
            xmp.set("dc:title<tag", "value").is_err(),
            "Key with < should fail"
        );
        assert!(
            xmp.set("dc:title>tag", "value").is_err(),
            "Key with > should fail"
        );

        // These should succeed
        assert!(xmp.set("dc:title", "value").is_ok(), "Normal key should work");
        assert!(
            xmp.set("dc:subject", "value").is_ok(),
            "Another normal key should work"
        );
        assert!(
            xmp.set("my_custom:field", "value").is_ok(),
            "Underscore key should work"
        );
    }

    #[test]
    fn test_multiple_rdf_description_blocks() {
        // XMP with multiple rdf:Description blocks (common in Adobe files)
        let xmp_str = r#"<?xml version="1.0"?>
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

        let xmp = MiniXmp::new(xmp_str);

        // Test that we can read from all blocks
        let values = xmp.get_many(&[
            "dc:title",
            "dc:creator",
            "photoshop:DateCreated",
            "dc:subject",
        ]);

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
        assert_eq!(xmp.get("dc:title"), Some("Second Block Title".to_string()));
        assert_eq!(xmp.get("dc:creator"), Some("Alice".to_string()));
        assert_eq!(xmp.get("photoshop:DateCreated"), Some("2024-01-15".to_string()));
    }

    #[test]
    fn test_write_only_modifies_first_block() {
        // XMP with multiple blocks
        let xmp = MiniXmp::new(r#"<rdf:RDF>
  <rdf:Description dc:title="First" dc:creator="Alice" />
  <rdf:Description photoshop:DateCreated="2024-01-15" />
</rdf:RDF>"#);

        // Modify a key
        let updated = xmp.apply_updates(&[("dc:title", Some("Modified"))]).unwrap();

        // First block should be modified
        assert!(
            updated.as_ref().contains("dc:title=\"Modified\""),
            "First block should be updated"
        );

        // Second block should be unchanged
        assert!(
            updated.as_ref().contains("photoshop:DateCreated=\"2024-01-15\""),
            "Second block should be preserved"
        );

        // Verify we can still read from both blocks
        let values = updated.get_many(&["dc:title", "photoshop:DateCreated"]);
        assert_eq!(values[0], Some("Modified".to_string()));
        assert_eq!(values[1], Some("2024-01-15".to_string()));
    }

    #[test]
    fn test_contains() {
        let xmp = MiniXmp::new(TEST_XMP);
        assert!(xmp.contains("dc:format"));
        assert!(!xmp.contains("nonexistent"));
    }

    #[test]
    fn test_into_inner() {
        let xmp = MiniXmp::new(TEST_XMP);
        let inner: String = xmp.into_inner();
        assert!(inner.contains("dc:format"));
    }

    #[test]
    fn test_as_ref() {
        let xmp = MiniXmp::new(TEST_XMP);
        let s: &str = xmp.as_ref();
        assert!(s.contains("dc:format"));
    }
}

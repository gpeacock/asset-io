//! Minimal XMP parser
//!
//! This module provides just enough XMP parsing to extract and modify simple key/value pairs
//! in XMP metadata for testing and basic operations.
//!
//! XMP Structure:
//! - XMP packets are XML-based RDF metadata
//! - Properties can be attributes on rdf:Description or child elements
//! - Packet padding maintains 2-4KB minimum size per spec
//!
//! # API Design
//!
//! ## Batch Operations (Efficient)
//!
//! For multiple keys, use batch operations to parse once:
//!
//! ```
//! use asset_io::xmp::{get_keys, apply_updates};
//!
//! let xmp = r#"<rdf:Description dc:title="Photo" dc:creator="John" />"#;
//! 
//! // Get multiple values in one pass
//! let values = get_keys(xmp, &["dc:title", "dc:creator", "dc:format"]);
//! assert_eq!(values[0], Some("Photo".to_string()));
//! assert_eq!(values[1], Some("John".to_string()));
//! assert_eq!(values[2], None);
//!
//! // Apply multiple updates in one pass
//! let updates = [
//!     ("dc:title", Some("New Photo")),
//!     ("dc:subject", Some("Landscape")),
//!     ("dc:creator", None),  // None = remove
//! ];
//! let updated = apply_updates(xmp, &updates).unwrap();
//! ```
//!
//! ## Single Operations (Convenience)
//!
//! For 1-2 operations, use convenience functions:
//!
//! ```
//! use asset_io::xmp::{get_key, add_key, remove_key};
//!
//! let xmp = r#"<rdf:Description dc:title="Photo" />"#;
//! let title = get_key(xmp, "dc:title");
//! let xmp = add_key(xmp, "dc:creator", "John").unwrap();
//! let xmp = remove_key(&xmp, "dc:title").unwrap();
//! ```

use crate::error::Result;
use std::io::Cursor;

const RDF_DESCRIPTION: &[u8] = b"rdf:Description";

/// Validate that a key is a reasonable XMP property name.
/// 
/// This is a basic check - it doesn't validate against full XMP spec,
/// just catches obvious errors like empty keys or keys with spaces/quotes.
fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(crate::Error::InvalidFormat("XMP key cannot be empty".to_string()));
    }
    
    // Check for characters that would break XML attribute syntax
    if key.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>') {
        return Err(crate::Error::InvalidFormat(
            format!("Invalid XMP key '{}': contains whitespace or XML special characters", key)
        ));
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
    
    // Track which keys we've found
    let mut results = vec![None; keys.len()];
    let mut found_count = 0;
    
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if e.name() == QName(RDF_DESCRIPTION) {
                    // Search attributes
                    for attr_result in e.attributes() {
                        if let Ok(attr) = attr_result {
                            // Check if this attribute matches any of our keys
                            for (i, key) in keys.iter().enumerate() {
                                if results[i].is_none() && attr.key == QName(key.as_bytes()) {
                                    // Use decode_and_unescape_value to handle XML entities
                                    if let Ok(s) = attr.decode_and_unescape_value(reader.decoder()) {
                                        results[i] = Some(s.to_string());
                                        found_count += 1;
                                        
                                        // Early exit if we found everything
                                        if found_count == keys.len() {
                                            return results;
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // Search as element
                    for (i, key) in keys.iter().enumerate() {
                        if results[i].is_none() && e.name() == QName(key.as_bytes()) {
                            if let Ok(s) = reader.read_text(e.name()) {
                                results[i] = Some(s.to_string());
                                found_count += 1;
                                if found_count == keys.len() {
                                    return results;
                                }
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
    use quick_xml::{events::{BytesStart, Event}, name::QName, Reader, Writer};
    
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
        assert_eq!(get_key(TEST_XMP, "dc:format"), Some("image/jpeg".to_string()));
        assert_eq!(get_key(TEST_XMP, "xmpMM:DocumentID"), Some("xmp.did:1234".to_string()));
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
        assert_eq!(get_key(&xmp, "xmpMM:DocumentID"), Some("xmp.did:1234".to_string()));
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
            ("dc:subject", Some("Landscape")),  // Add new
        ];
        let xmp = apply_updates(TEST_XMP, &updates).unwrap();
        
        assert_eq!(get_key(&xmp, "dc:title"), Some("My Photo".to_string()));
        assert_eq!(get_key(&xmp, "dc:format"), Some("image/png".to_string()));
        assert_eq!(get_key(&xmp, "dc:subject"), Some("Landscape".to_string()));
        // Original key should still be there
        assert_eq!(get_key(&xmp, "xmpMM:DocumentID"), Some("xmp.did:1234".to_string()));
    }

    #[test]
    fn test_apply_updates_remove() {
        let updates = [
            ("dc:format", None),  // Remove
            ("dc:title", Some("New")),  // Add
        ];
        let xmp = apply_updates(TEST_XMP, &updates).unwrap();
        
        assert_eq!(get_key(&xmp, "dc:format"), None);
        assert_eq!(get_key(&xmp, "dc:title"), Some("New".to_string()));
        assert_eq!(get_key(&xmp, "xmpMM:DocumentID"), Some("xmp.did:1234".to_string()));
    }

    #[test]
    fn test_apply_updates_mixed() {
        let updates = [
            ("dc:format", Some("image/png")),     // Replace
            ("dc:title", Some("Photo")),          // Add
            ("xmpMM:DocumentID", None),           // Remove
            ("dc:creator", Some("John")),         // Add
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
        assert!(add_key(xmp, "dc:title with space", "value").is_err(), "Key with space should fail");
        assert!(add_key(xmp, "dc:title\"quote", "value").is_err(), "Key with quote should fail");
        assert!(add_key(xmp, "dc:title<tag", "value").is_err(), "Key with < should fail");
        assert!(add_key(xmp, "dc:title>tag", "value").is_err(), "Key with > should fail");
        
        // These should succeed
        assert!(add_key(xmp, "dc:title", "value").is_ok(), "Normal key should work");
        assert!(add_key(xmp, "dc:subject", "value").is_ok(), "Another normal key should work");
        assert!(add_key(xmp, "my_custom:field", "value").is_ok(), "Underscore key should work");
    }
}


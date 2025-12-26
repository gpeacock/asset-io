//! Minimal XMP parser
//!
//! This module provides just enough XMP parsing to extract and modify simple key/value pairs
//! in XMP metadata for testing and basic operations.
//!
//! XMP Structure:
//! - XMP packets are XML-based RDF metadata
//! - Properties can be attributes on rdf:Description or child elements
//! - Packet padding maintains 2-4KB minimum size per spec

use crate::error::Result;
use std::io::Cursor;

const RDF_DESCRIPTION: &[u8] = b"rdf:Description";

/// Extract a value from XMP using a key.
///
/// Searches for the key as an attribute on `rdf:Description` or as a child element.
///
/// # Example
///
/// ```
/// use asset_io::xmp::extract_key;
///
/// let xmp = r#"<rdf:Description dc:title="My Photo" />"#;
/// assert_eq!(extract_key(xmp, "dc:title"), Some("My Photo".to_string()));
/// ```
pub fn extract_key(xmp: &str, key: &str) -> Option<String> {
    use quick_xml::{
        events::Event,
        name::QName,
        Reader,
    };
    
    let mut reader = Reader::from_str(xmp);
    reader.config_mut().trim_text(true);

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if e.name() == QName(RDF_DESCRIPTION) {
                    // Search attributes
                    for attr_result in e.attributes() {
                        if let Ok(attr) = attr_result {
                            if attr.key == QName(key.as_bytes()) {
                                if let Ok(s) = String::from_utf8(attr.value.to_vec()) {
                                    return Some(s);
                                }
                            }
                        }
                    }
                } else if e.name() == QName(key.as_bytes()) {
                    // Search as element
                    if let Ok(s) = reader.read_text(e.name()) {
                        return Some(s.to_string());
                    }
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }
    None
}

/// Add or replace a key/value pair in XMP.
///
/// If the key exists as an attribute on `rdf:Description`, it will be replaced.
/// Otherwise, it will be added as a new attribute.
///
/// # Example
///
/// ```
/// use asset_io::xmp::{add_key, extract_key};
///
/// let xmp = r#"<?xpacket begin=""?><rdf:RDF><rdf:Description /></rdf:RDF><?xpacket end="w"?>"#;
/// let updated = add_key(xmp, "dc:title", "My Photo").unwrap();
/// assert!(updated.contains(r#"dc:title="My Photo""#));
/// ```
pub fn add_key(xmp: &str, key: &str, value: &str) -> Result<String> {
    use quick_xml::{
        events::{BytesStart, Event},
        name::QName,
        Reader, Writer,
    };
    
    let mut reader = Reader::from_str(xmp);
    reader.config_mut().trim_text(false);
    reader.config_mut().expand_empty_elements = false;
    
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut added = false;
    
    loop {
        let event = reader.read_event()?;
        match event {
            Event::Start(ref e) if e.name() == QName(RDF_DESCRIPTION) && !added => {
                let mut elem = BytesStart::from_content(
                    String::from_utf8_lossy(RDF_DESCRIPTION),
                    RDF_DESCRIPTION.len(),
                );
                
                for attr_result in e.attributes() {
                    match attr_result {
                        Ok(attr) => {
                            if attr.key == QName(key.as_bytes()) {
                                elem.push_attribute((key, value));
                                added = true;
                            } else {
                                elem.extend_attributes([attr]);
                            }
                        }
                        Err(e) => return Err(crate::Error::InvalidFormat(format!("XMP attribute error: {}", e))),
                    }
                }
                
                if !added {
                    elem.push_attribute((key, value));
                    added = true;
                }
                
                writer.write_event(Event::Start(elem))?;
            }
            Event::Empty(ref e) if e.name() == QName(RDF_DESCRIPTION) && !added => {
                let mut elem = BytesStart::from_content(
                    String::from_utf8_lossy(RDF_DESCRIPTION),
                    RDF_DESCRIPTION.len(),
                );
                
                for attr_result in e.attributes() {
                    match attr_result {
                        Ok(attr) => {
                            if attr.key == QName(key.as_bytes()) {
                                elem.push_attribute((key, value));
                                added = true;
                            } else {
                                elem.extend_attributes([attr]);
                            }
                        }
                        Err(e) => return Err(crate::Error::InvalidFormat(format!("XMP attribute error: {}", e))),
                    }
                }
                
                if !added {
                    elem.push_attribute((key, value));
                    added = true;
                }
                
                writer.write_event(Event::Empty(elem))?;
            }
            Event::Eof => break,
            e => writer.write_event(e)?,
        }
    }
    
    let result = writer.into_inner().into_inner();
    String::from_utf8(result).map_err(|e| crate::Error::InvalidFormat(e.to_string()))
}

/// Remove a key from XMP.
///
/// Removes the attribute from `rdf:Description` if it exists.
///
/// # Example
///
/// ```
/// use asset_io::xmp::{remove_key, extract_key};
///
/// let xmp = r#"<rdf:Description dc:title="My Photo" dc:creator="John" />"#;
/// let updated = remove_key(xmp, "dc:title").unwrap();
/// assert_eq!(extract_key(&updated, "dc:title"), None);
/// assert_eq!(extract_key(&updated, "dc:creator"), Some("John".to_string()));
/// ```
pub fn remove_key(xmp: &str, key: &str) -> Result<String> {
    use quick_xml::{
        events::{BytesStart, Event},
        name::QName,
        Reader, Writer,
    };
    
    let mut reader = Reader::from_str(xmp);
    reader.config_mut().trim_text(false);
    reader.config_mut().expand_empty_elements = false;
    
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    
    loop {
        let event = reader.read_event()?;
        match event {
            Event::Start(ref e) if e.name() == QName(RDF_DESCRIPTION) => {
                let mut elem = BytesStart::from_content(
                    String::from_utf8_lossy(RDF_DESCRIPTION),
                    RDF_DESCRIPTION.len(),
                );
                
                for attr_result in e.attributes() {
                    match attr_result {
                        Ok(attr) => {
                            // Skip the key we want to remove
                            if attr.key != QName(key.as_bytes()) {
                                elem.extend_attributes([attr]);
                            }
                        }
                        Err(e) => return Err(crate::Error::InvalidFormat(format!("XMP attribute error: {}", e))),
                    }
                }
                
                writer.write_event(Event::Start(elem))?;
            }
            Event::Empty(ref e) if e.name() == QName(RDF_DESCRIPTION) => {
                let mut elem = BytesStart::from_content(
                    String::from_utf8_lossy(RDF_DESCRIPTION),
                    RDF_DESCRIPTION.len(),
                );
                
                for attr_result in e.attributes() {
                    match attr_result {
                        Ok(attr) => {
                            // Skip the key we want to remove
                            if attr.key != QName(key.as_bytes()) {
                                elem.extend_attributes([attr]);
                            }
                        }
                        Err(e) => return Err(crate::Error::InvalidFormat(format!("XMP attribute error: {}", e))),
                    }
                }
                
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
    fn test_extract_key() {
        assert_eq!(extract_key(TEST_XMP, "dc:format"), Some("image/jpeg".to_string()));
        assert_eq!(extract_key(TEST_XMP, "xmpMM:DocumentID"), Some("xmp.did:1234".to_string()));
        assert_eq!(extract_key(TEST_XMP, "nonexistent"), None);
    }

    #[test]
    fn test_add_key() {
        let xmp = add_key(TEST_XMP, "dc:title", "My Photo").unwrap();
        assert_eq!(extract_key(&xmp, "dc:title"), Some("My Photo".to_string()));
        // Original keys should still be there
        assert_eq!(extract_key(&xmp, "dc:format"), Some("image/jpeg".to_string()));
    }

    #[test]
    fn test_replace_key() {
        let xmp = add_key(TEST_XMP, "dc:format", "image/png").unwrap();
        assert_eq!(extract_key(&xmp, "dc:format"), Some("image/png".to_string()));
    }

    #[test]
    fn test_remove_key() {
        let xmp = remove_key(TEST_XMP, "dc:format").unwrap();
        assert_eq!(extract_key(&xmp, "dc:format"), None);
        // Other keys should still be there
        assert_eq!(extract_key(&xmp, "xmpMM:DocumentID"), Some("xmp.did:1234".to_string()));
    }

    #[test]
    fn test_remove_nonexistent_key() {
        let xmp = remove_key(TEST_XMP, "nonexistent").unwrap();
        // Should not error, just return unchanged XMP
        assert_eq!(extract_key(&xmp, "dc:format"), Some("image/jpeg".to_string()));
    }
}


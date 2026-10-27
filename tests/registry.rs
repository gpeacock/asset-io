use asset_io::{
    BoxFuture, ContainerHandler, Error, FormatSpec, HandlerRegistry, Io, Result, Structure,
};
use std::io::Cursor;
use std::sync::Arc;

#[derive(Debug)]
struct Custom;

impl ContainerHandler for Custom {
    fn id(&self) -> &'static str {
        "custom"
    }

    fn formats(&self) -> &[FormatSpec] {
        const FORMATS: &[FormatSpec] = &[FormatSpec::new("application/x-custom", "custom")];
        FORMATS
    }

    fn parse_async(self: Arc<Self>, _: Io) -> BoxFuture<'static, Result<Structure>> {
        Box::pin(async { Err(Error::UnsupportedFeature("test handler")) })
    }
}

#[test]
fn builtin_registry_contains_exactly_the_enabled_families() {
    let registry = HandlerRegistry::builtin();
    assert!(Arc::ptr_eq(&registry, &HandlerRegistry::builtin()));
    let families = [
        ("jpg", cfg!(feature = "jpeg"), "jpeg"),
        ("png", cfg!(feature = "png"), "png"),
        ("heic", cfg!(feature = "bmff"), "bmff"),
        ("webp", cfg!(feature = "riff"), "riff"),
        ("svg", cfg!(feature = "svg"), "svg"),
        ("pdf", cfg!(feature = "pdf"), "pdf"),
        ("c2pa", cfg!(feature = "c2pa"), "c2pa"),
    ];
    for (extension, enabled, id) in families {
        assert_eq!(
            registry.container_for(extension),
            enabled.then_some(id),
            "{extension}"
        );
    }
    if families.iter().all(|(_, enabled, _)| !enabled) {
        assert!(registry.formats().is_empty());
        assert!(registry.detect(b"\xff\xd8\xff\xe0").is_none());
        assert!(matches!(
            registry.parse("jpg", &mut Cursor::new([])),
            Err(Error::UnsupportedFormat)
        ));
        assert!(matches!(
            registry.parse_detect(&mut Cursor::new(b"%PDF-1.7\n")),
            Err(Error::UnsupportedFormat)
        ));
    }
}

#[test]
fn new_and_default_are_empty_even_when_builtins_are_enabled() {
    assert!(HandlerRegistry::new().formats().is_empty());
    assert!(HandlerRegistry::default().formats().is_empty());
}

#[test]
fn custom_registration_and_fallback_preserve_core_interoperability() {
    let builtin = HandlerRegistry::builtin();
    let registry = HandlerRegistry::extending_builtin().with_handler(Custom);
    assert!(registry.handler("custom").is_some());
    assert!(builtin.handler("custom").is_none());
    for format in builtin.formats() {
        assert_eq!(
            registry.container_for(&format),
            builtin.container_for(&format)
        );
    }
    let base = Arc::new(registry);
    let mut fallback = HandlerRegistry::with_fallback(Arc::clone(&base));
    assert!(fallback.handler("custom").is_some());
    fallback.add_arc(Arc::new(Custom));
    let core: &asset_io_core::HandlerRegistry = &fallback;
    assert_eq!(core.container_for("custom"), Some("custom"));
    assert!(matches!(
        core.parse("custom", &mut Cursor::new([])),
        Err(Error::UnsupportedFeature("test handler"))
    ));
}

#[test]
fn an_existing_fallback_is_not_changed_by_later_mutations() {
    let mut base = Arc::new(HandlerRegistry::new());
    let fallback = HandlerRegistry::with_fallback(Arc::clone(&base));
    Arc::get_mut(&mut base).unwrap().add_handler(Custom);
    assert!(base.handler("custom").is_some());
    assert!(fallback.handler("custom").is_none());
}

#[cfg(feature = "c2pa")]
#[test]
fn sidecars_work_through_the_feature_selected_registry() {
    let registry = HandlerRegistry::builtin();
    let mut source = Cursor::new(b"raw manifest-store bytes");
    let structure = registry
        .parse("application/x-c2pa-manifest-store", &mut source)
        .unwrap();
    assert_eq!(structure.container_id(), asset_io::containers::c2pa::ID);
    assert_eq!(
        structure.store(&mut source).unwrap().unwrap(),
        source.get_ref().as_slice()
    );
    assert_eq!(
        registry
            .compose("c2pa", asset_io::ItemKind::Jumbf, b"replacement")
            .unwrap(),
        b"replacement"
    );
    let mut output = Cursor::new(Vec::new());
    let result = structure
        .write(
            &mut source,
            &mut output,
            &asset_io::Updates::new().remove_jumbf(),
            &mut asset_io::NullSink,
        )
        .unwrap();
    assert!(output.into_inner().is_empty());
    assert!(result.items().next().is_none());
}

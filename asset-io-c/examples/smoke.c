#include "asset_io.h"
#include <assert.h>
#include <string.h>

static const unsigned char jpeg[] = {0xff, 0xd8, 0xff, 0xd9};

int main(void) {
    assert(asset_io_abi_version() == 1);
    AssetIoRegistry *registry = asset_io_registry_new(1);
    assert(registry);
    AssetIoAsset *asset = asset_io_parse_memory(registry, "jpg", jpeg, sizeof jpeg);
    assert(asset);
    AssetIoUpdates *updates = asset_io_updates_new();
    const unsigned char xmp[] = "<x:xmpmeta/>";
    assert(asset_io_updates_metadata(updates, 1, 1, xmp, sizeof xmp - 1) == 0);
    AssetIoBuffer *output = asset_io_write_memory(asset, updates);
    assert(output);
    size_t len = 0;
    const unsigned char *bytes = asset_io_buffer_data(output, &len);
    assert(bytes && len > sizeof jpeg);
    AssetIoAsset *reparsed = asset_io_parse_memory(registry, NULL, bytes, len);
    assert(reparsed);
    AssetIoBuffer *metadata = asset_io_metadata_bytes(reparsed, 1);
    assert(metadata);
    bytes = asset_io_buffer_data(metadata, &len);
    assert(len == sizeof xmp - 1 && memcmp(bytes, xmp, len) == 0);
    AssetIoStructure *structure = asset_io_asset_structure(reparsed);
    AssetIoBuffer *json = asset_io_structure_json(structure);
    assert(json);
    assert(asset_io_free(json) == 0);
    assert(asset_io_free(structure) == 0);
    assert(asset_io_free(metadata) == 0);
    assert(asset_io_free(reparsed) == 0);
    assert(asset_io_free(output) == 0);
    assert(asset_io_free(updates) == 0);
    assert(asset_io_free(asset) == 0);
    assert(asset_io_free(registry) == 0);
    assert(asset_io_free(NULL) == 0);
    return 0;
}

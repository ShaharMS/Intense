//! Normalize both request forms before the shared progressive decoder runs.
use std::collections::{BTreeMap, BTreeSet};

use river_core::{AssetManifest, ClientMessage, ModelError, TILE_SIZE};

pub struct TileGroup {
    pub zoom: u8,
    pub tiles: Vec<(u32, u32)>,
}

/// Whether this normalized group actually covers the complete raster grid.
/// Request syntax alone must not decide whether index reads can also fetch
/// prefixes needed by every spatial tile in the request.
pub fn covers_image(group:&TileGroup,manifest:&AssetManifest)->bool {
    let Some(edge)=TILE_SIZE.checked_shl(u32::from(group.zoom)).filter(|&n|n>0) else{return false;};
    if manifest.width==0 || manifest.height==0 {return false;}
    let columns=manifest.width.div_ceil(edge);let rows=manifest.height.div_ceil(edge);
    let expected=u64::from(columns)*u64::from(rows);
    group.tiles.len() as u64==expected && group.tiles.iter().all(|&(x,y)|x<columns && y<rows)
        && group.tiles.iter().copied().collect::<BTreeSet<_>>().len()==group.tiles.len()
}

pub fn plan(
    request: &ClientMessage,
    manifest: &AssetManifest,
) -> Result<Vec<TileGroup>, ModelError> {
    match request {
        ClientMessage::Viewport { viewport, .. } => {
            viewport.validate_against(manifest)?;
            Ok(vec![TileGroup {
                zoom: viewport.zoom,
                tiles: super::tiles_for(viewport),
            }])
        }
        ClientMessage::Tiles { batch, .. } => {
            // Validate the whole batch before acceptance or any decoder work.
            batch.validate_against(manifest)?;
            let mut levels = BTreeMap::<u8, BTreeSet<(u32, u32)>>::new();
            for tile in &batch.tiles {
                levels.entry(tile.z).or_default().insert((tile.x, tile.y));
            }
            Ok(levels
                .into_iter()
                .map(|(zoom, requested)| {
                    let requested = requested.into_iter().collect::<Vec<_>>();
                    let mut tiles = BTreeSet::new();
                    // Fill only holes inside each occupied 2x2 decode union. A
                    // sparse batch never creates a whole-image bounding rectangle.
                    // Every extra tile shares decoding already needed by the batch.
                    for union in super::tile_batches(&requested) {
                        let min_x = union.iter().map(|tile| tile.0).min().unwrap();
                        let max_x = union.iter().map(|tile| tile.0).max().unwrap();
                        let min_y = union.iter().map(|tile| tile.1).min().unwrap();
                        let max_y = union.iter().map(|tile| tile.1).max().unwrap();
                        for y in min_y..=max_y {
                            for x in min_x..=max_x {
                                tiles.insert((x, y));
                            }
                        }
                    }
                    let mut tiles = tiles.into_iter().collect::<Vec<_>>();
                    // Preserve viewport row ordering for identical decoder jobs.
                    tiles.sort_unstable_by_key(|&(x, y)| (y, x));
                    debug_assert!(tiles.len() <= batch.tiles.len() * 2);
                    debug_assert!(TILE_SIZE.checked_shl(u32::from(zoom)).is_some());
                    TileGroup { zoom, tiles }
                })
                .collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use river_core::{AssetId, MAX_BATCH_TILES, TileBatch, TileXyz, Viewport};

    fn manifest() -> AssetManifest {
        let mut manifest = crate::default_asset().manifest;
        manifest.width = 10_001;
        manifest.height = 8_001;
        manifest
    }

    fn request(tiles: &[(u32, u32, u8)]) -> ClientMessage {
        ClientMessage::Tiles {
            batch: TileBatch {
                asset_id: manifest().asset_id,
                sequence: 42,
                tiles: tiles.iter().map(|&(x, y, z)| TileXyz { x, y, z }).collect(),
            },
            format: river_core::RasterFormat::default(),
            driver: river_core::DecoderDriver::default(),
        }
    }

    #[test]
    fn rectangular_batch_matches_viewport_jobs() {
        let manifest = manifest();
        let viewport = ClientMessage::Viewport { progressive_patches:false,
            viewport: Viewport {
                asset_id: manifest.asset_id.clone(),
                sequence: 42,
                x: 1024,
                y: 2048,
                width: 1536,
                height: 1024,
                zoom: 0,
            },
            format: river_core::RasterFormat::default(),
            driver: river_core::DecoderDriver::default(),
        };
        let batch = request(&[
            (4, 5, 0),
            (2, 4, 0),
            (3, 5, 0),
            (4, 4, 0),
            (2, 5, 0),
            (3, 4, 0),
        ]);
        let v = plan(&viewport, &manifest).unwrap();
        let b = plan(&batch, &manifest).unwrap();
        assert_eq!(v[0].tiles, b[0].tiles);
        assert_eq!(
            crate::tile_batches(&v[0].tiles),
            crate::tile_batches(&b[0].tiles)
        );
    }

    #[test]
    fn sparse_unsorted_duplicates_and_mixed_zoom_are_bounded() {
        let groups = plan(
            &request(&[(19, 15, 0), (1, 1, 0), (0, 0, 0), (1, 1, 0), (0, 0, 2)]),
            &manifest(),
        )
        .unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups[0].tiles,
            vec![(0, 0), (1, 0), (0, 1), (1, 1), (19, 15)]
        );
        assert_eq!(groups[1].zoom, 2);
        assert_eq!(groups[1].tiles, vec![(0, 0)]);
    }

    #[test]
    fn invalid_batch_is_rejected_atomically() {
        for tiles in [
            vec![],
            vec![(0, 0, 0); MAX_BATCH_TILES + 1],
            vec![(0, 0, 0), (20, 0, 0)],
            vec![(u32::MAX, 0, 0)],
            vec![(0, 0, 32)],
        ] {
            assert!(plan(&request(&tiles), &manifest()).is_err());
        }
        let mut batch = TileBatch {
            asset_id: AssetId("bad".into()),
            sequence: 1,
            tiles: vec![TileXyz { x: 0, y: 0, z: 0 }],
        };
        assert!(batch.validate_against(&manifest()).is_err());
        batch.asset_id = manifest().asset_id;
        let mut oversized_zoom = manifest();
        oversized_zoom.resolution_levels = 32;
        batch.tiles[0].z = 23;
        assert!(batch.validate_against(&oversized_zoom).is_err());
    }

    #[test]
    fn whole_grid_coverage_is_geometric_and_sparse_batches_stay_partial() {
        let mut m=manifest();m.width=40980;m.height=10980;m.resolution_levels=7;
        let full=plan(&request(&[(0,0,5),(1,0,5),(2,0,5)]),&m).unwrap();
        assert!(covers_image(&full[0],&m));
        let sparse=plan(&request(&[(0,0,5),(2,0,5)]),&m).unwrap();assert!(!covers_image(&sparse[0],&m));
        assert!(!covers_image(&TileGroup {zoom:5,tiles:vec![(0,0),(0,0),(2,0)]},&m));
        assert!(!covers_image(&TileGroup {zoom:5,tiles:vec![(0,0),(1,0),(3,0)]},&m));
        assert!(!covers_image(&TileGroup {zoom:23,tiles:vec![(0,0)]},&m));
    }

    #[test]
    fn partial_edge_tiles_are_valid() {
        assert!(plan(&request(&[(19, 15, 0), (9, 7, 1)]), &manifest()).is_ok());
    }
}

#[cfg(test)]
mod patch_wire_tests {
    #[test]
    fn camel_case_patch_opt_in_is_not_silently_ignored() {
        let value=serde_json::json!({"type":"viewport","viewport":{"assetId":"fixture","sequence":1,"x":0,"y":0,"width":512,"height":512,"zoom":0},"progressivePatches":true});
        let request:river_core::ClientMessage=serde_json::from_value(value).unwrap();
        assert!(matches!(request,river_core::ClientMessage::Viewport {progressive_patches:true,..}));
    }
}

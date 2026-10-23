use super::*;

#[test]
fn imports_native_parent_models_slab_geometry_and_uv_locked_rotation() {
    let pack = Resources::load(&fixture::bundle()).unwrap();
    assert_eq!(pack.registry.len(), 8);
    assert!(pack.is_air(0));
    assert!(pack.is_air(4));
    assert!(!pack.is_air(1));
    let cube = pack.faces(2, BlockPos::new(0, 0, 0));
    assert_eq!(cube.len(), 6);
    assert!(cube.iter().all(|f| f.tile > 0));
    let slab = pack.faces(3, BlockPos::new(0, 0, 0));
    assert!(
        slab.iter()
            .flat_map(|f| f.quad.positions)
            .all(|p| p[1] <= 0.5)
    );
    let steps = pack.faces(7, BlockPos::new(0, 0, 0));
    assert_eq!(steps.len(), 12);
    assert!(
        steps
            .iter()
            .flat_map(|f| f.quad.positions)
            .any(|p| p[0] == 0.5)
    );
    assert!(
        pack.atlas
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| u16::from(p[0]) > u16::from(p[1]) * 2)
    );
}

#[test]
fn full_opaque_faces_occlude_but_partial_and_cutout_faces_do_not() {
    let pack = Resources::load(&fixture::bundle()).unwrap();
    assert!(!pack.visible(1, 2, Direction::East));
    assert!(pack.visible(1, 3, Direction::Down));
    assert!(pack.visible(1, 4, Direction::East));
    assert!(pack.visible(1, 5, Direction::East));
    assert!(
        pack.faces(6, BlockPos::new(0, 0, 0))
            .iter()
            .all(|f| f.translucent)
    );
}

#[test]
fn refuses_wrong_version_and_unsafe_archive_paths() {
    assert!(!allowed_path("../facts.json"));
    assert!(!allowed_path("assets/horizon/../../textures/a.png"));
    let bad = fixture::zip(vec![(
        "horizon-resources.json".into(),
        br#"{"data_version":0}"#.to_vec(),
    )]);
    assert!(
        Resources::load(&bad)
            .err()
            .unwrap()
            .contains("data version")
    );
    let unsafe_zip = fixture::zip(vec![("../facts.json".into(), vec![0])]);
    assert!(Resources::load(&unsafe_zip).err().unwrap().contains("path"));
}

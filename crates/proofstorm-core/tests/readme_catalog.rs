//! Keep the short public support matrix aligned with the shipped catalog.
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn readme_lists_every_catalog_component_and_version() {
    let readme = include_str!("../../../README.md");
    let matrix = readme
        .split("## Components\n")
        .nth(1)
        .unwrap()
        .split("## CLI")
        .next()
        .unwrap();
    let mut actual = BTreeMap::new();
    for row in matrix.lines() {
        let cells: Vec<_> = row.split('|').map(str::trim).collect();
        if cells.len() != 6 || !cells[2].starts_with('`') {
            continue;
        }
        let id = cells[2].trim_matches('`').to_owned();
        let versions: BTreeSet<_> = cells[3].split(';').map(|v| v.trim().to_owned()).collect();
        assert!(
            actual.insert(id, versions).is_none(),
            "duplicate README component row"
        );
    }
    let mut expected: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for entry in &proofstorm_core::default_catalog().entries {
        expected
            .entry(entry.id.clone())
            .or_default()
            .insert(entry.version.clone());
    }
    assert_eq!(
        actual, expected,
        "update the README component matrix alongside the catalog"
    );
}

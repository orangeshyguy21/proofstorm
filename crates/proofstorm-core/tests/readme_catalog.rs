//! Keep the short public support matrix aligned with the shipped catalog.
use std::collections::BTreeSet;

#[test]
fn readme_lists_every_catalog_component() {
    let readme = include_str!("../../../README.md");
    let matrix = readme
        .split("## Components\n")
        .nth(1)
        .unwrap()
        .split("\n## ")
        .next()
        .unwrap();
    let mut actual = BTreeSet::new();
    for row in matrix.lines() {
        let cells: Vec<_> = row.split('|').map(str::trim).collect();
        if cells.len() != 5 || !cells[2].starts_with('`') {
            continue;
        }
        let id = cells[2].trim_matches('`').to_owned();
        assert!(actual.insert(id), "duplicate README component row");
    }
    let expected: BTreeSet<_> = proofstorm_core::default_catalog()
        .entries
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    assert_eq!(
        actual, expected,
        "update the README component matrix alongside the catalog"
    );
}

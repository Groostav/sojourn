//! The README's install line is the first thing a newcomer copies, and nothing
//! else notices when it goes stale: it said `git = ..., tag = "v0.2.0"` for two
//! releases after that stopped being true. So the manifest's version is the
//! spec, and every `sojourn = ...` line in the README must name its
//! `major.minor` — a caret requirement, which before 1.0 moves only on a
//! breaking release, so a patch bump never turns this red.

use std::path::Path;

#[test]
fn when_readme_names_the_dependency_should_require_the_manifests_major_minor() {
    // setup
    let readme = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
        .expect("README.md sits beside Cargo.toml");
    let version = env!("CARGO_PKG_VERSION");
    let [major, minor, _patch] = version.splitn(3, '.').collect::<Vec<_>>()[..] else {
        panic!("Cargo.toml's version {version:?} is not major.minor.patch")
    };
    let expected = format!(r#"sojourn = "{major}.{minor}""#);

    // act
    let dependency_lines: Vec<&str> = readme
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("sojourn ="))
        .collect();

    // assert
    assert!(
        !dependency_lines.is_empty(),
        "README.md has no `sojourn = ...` line to install from; expected `{expected}`"
    );
    for line in dependency_lines {
        assert_eq!(
            line, expected,
            "README.md's install line disagrees with Cargo.toml's version {version}"
        );
    }
}

mod common;

use herdr_file_viewer::index;
use std::fs;

// (a) AC-12: a file nested >= 2 levels deep appears (recursive walk)
#[test]
fn nested_file_appears() {
    let tmp = common::TempDir::new();
    let root = tmp.path();
    fs::create_dir_all(root.join("a/b")).unwrap();
    fs::write(root.join("a/b/deep.rs"), "").unwrap();

    let paths = index::build(root);
    assert!(
        paths.iter().any(|p| p == "a/b/deep.rs"),
        "expected a/b/deep.rs in index, got: {paths:?}"
    );
}

// (b) AC-13: a .gitignored file is absent
#[test]
fn gitignored_file_is_absent() {
    let tmp = common::TempDir::new();
    let root = tmp.path();
    fs::write(root.join(".gitignore"), "secret.txt\n").unwrap();
    fs::write(root.join("secret.txt"), "hidden").unwrap();
    fs::write(root.join("visible.txt"), "shown").unwrap();

    let paths = index::build(root);
    assert!(
        !paths.iter().any(|p| p == "secret.txt"),
        "secret.txt must be absent (gitignored)"
    );
    assert!(
        paths.iter().any(|p| p == "visible.txt"),
        "visible.txt must be present"
    );
}

// (c) AC-14: nothing under .git/ appears
#[test]
fn git_subtree_is_excluded() {
    let tmp = common::TempDir::new();
    let root = tmp.path();
    common::init_repo_with_commit(root);

    let paths = index::build(root);
    assert!(
        !paths.iter().any(|p| p.starts_with(".git")),
        "no path may start with .git, got: {paths:?}"
    );
}

// (d) AC-15: directories are NOT in the list, only files
#[test]
fn directories_not_in_index() {
    let tmp = common::TempDir::new();
    let root = tmp.path();
    fs::create_dir_all(root.join("subdir")).unwrap();
    fs::write(root.join("subdir/file.txt"), "").unwrap();

    let paths = index::build(root);
    assert!(
        !paths.iter().any(|p| p == "subdir"),
        "bare directory 'subdir' must not appear in index"
    );
    assert!(
        paths.iter().any(|p| p == "subdir/file.txt"),
        "subdir/file.txt must be in the index"
    );
}

// (e) AC-N5: every returned path is root-relative — no absolute paths, no ".."
#[test]
fn all_paths_are_root_relative() {
    let tmp = common::TempDir::new();
    let root = tmp.path();
    fs::create_dir_all(root.join("a/b")).unwrap();
    fs::write(root.join("a/b/deep.rs"), "").unwrap();
    fs::write(root.join("top.txt"), "").unwrap();

    let paths = index::build(root);
    assert!(!paths.is_empty(), "index must not be empty");
    for p in &paths {
        assert!(
            !std::path::Path::new(p).is_absolute(),
            "path must not be absolute: {p}"
        );
        assert!(!p.contains(".."), "path must not contain '..': {p}");
    }
}

// (f) AC-18: build is fresh each call — a new file added between calls appears
#[test]
fn rebuild_includes_new_file() {
    let tmp = common::TempDir::new();
    let root = tmp.path();
    fs::write(root.join("first.txt"), "").unwrap();

    let before = index::build(root);
    assert!(before.iter().any(|p| p == "first.txt"));
    assert!(!before.iter().any(|p| p == "second.txt"));

    fs::write(root.join("second.txt"), "").unwrap();
    let after = index::build(root);
    assert!(
        after.iter().any(|p| p == "second.txt"),
        "second.txt must appear after it is created"
    );
}

// (g) AC-19: works in a non-git directory without error
#[test]
fn works_in_non_git_dir() {
    let tmp = common::TempDir::new();
    let root = tmp.path();
    // No git init — plain directory
    fs::write(root.join("plain.txt"), "hello").unwrap();

    let paths = index::build(root);
    assert!(
        paths.iter().any(|p| p == "plain.txt"),
        "plain.txt must appear in a non-git dir"
    );
}

// (i) Regression: `build_scoped(root, true)` bounds the ancestor `.gitignore` search at root's
// own repo boundary instead of letting it climb into an unrelated enclosing directory. See the
// matching `tree_filters.rs` test for the Tree Model side of the same fix.
#[test]
fn build_scoped_bounds_ancestor_gitignore_at_the_repo_boundary() {
    let outer = common::TempDir::new();
    fs::write(outer.path().join(".gitignore"), "vendor/\n").unwrap();
    let inner = outer.path().join("inner");
    fs::create_dir_all(inner.join("vendor")).unwrap();
    common::init_repo_with_commit(&inner);
    fs::write(inner.join("vendor/keep.txt"), "k").unwrap();

    // `build` (is_git_repo = false, today's default) is unbounded and still picks up the outer
    // ancestor's unrelated `vendor/` rule.
    let unbounded = index::build(&inner);
    assert!(
        !unbounded.iter().any(|p| p.starts_with("vendor/")),
        "sanity check: the outer ancestor .gitignore reaches in when not told this is a repo"
    );

    // `build_scoped(&inner, true)` bounds the search at `inner`'s own `.git`.
    let bounded = index::build_scoped(&inner, true);
    assert!(
        bounded.iter().any(|p| p == "vendor/keep.txt"),
        "an unrelated ancestor .gitignore outside the repo must not hide files inside it, got: {bounded:?}"
    );
}

// (h) AC-N1: the filesystem is unchanged after build
#[test]
fn filesystem_unchanged_after_build() {
    let tmp = common::TempDir::new();
    let root = tmp.path();
    fs::write(root.join("file.txt"), "content").unwrap();

    let before: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();

    let _ = index::build(root);

    let after: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();

    assert_eq!(
        before.len(),
        after.len(),
        "build must not add/remove entries in root"
    );
}

// (i) #164: files under a symlinked directory are indexed when the link resolves inside the
// root, and never when it escapes it (AC-N5) — the finder follows the same rule as the tree.
#[cfg(unix)]
#[test]
fn follows_in_root_directory_symlinks_but_never_out_of_root_ones() {
    use std::os::unix::fs::symlink;
    let tmp = common::TempDir::new();
    let outside = common::TempDir::new();
    let root = tmp.path();
    fs::create_dir_all(root.join("real")).unwrap();
    fs::write(root.join("real/note.md"), "").unwrap();
    symlink(root.join("real"), root.join("link")).unwrap();
    fs::write(outside.path().join("secret.txt"), "").unwrap();
    symlink(outside.path(), root.join("escape")).unwrap();
    // A loop back to the root must not make the walk unbounded.
    symlink(".", root.join("real/loop")).unwrap();

    let paths = index::build(root);
    assert!(
        paths.iter().any(|p| p == "link/note.md"),
        "a file under an in-root symlinked dir is indexed: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.contains("secret.txt")),
        "nothing under an out-of-root symlink is indexed: {paths:?}"
    );
}

// (j) `follow_symlinks = true`: files under an out-of-root link are indexed under the LINK's path,
// and a loop back to an ancestor still does not make the walk unbounded.
#[cfg(unix)]
#[test]
fn follow_symlinks_indexes_out_of_root_links_under_the_link_path() {
    use std::os::unix::fs::symlink;
    let tmp = common::TempDir::new();
    let outside = common::TempDir::new();
    let root = tmp.path();
    fs::create_dir_all(outside.path().join("sub")).unwrap();
    fs::write(outside.path().join("sub/note.md"), "").unwrap();
    symlink(outside.path(), root.join("data")).unwrap();
    symlink(outside.path(), outside.path().join("sub/loop")).unwrap();

    assert!(
        !index::build(root).iter().any(|p| p.starts_with("data/")),
        "default: out-of-root link not followed"
    );
    let paths = index::build_following(root, true);
    assert!(
        paths.iter().any(|p| p == "data/sub/note.md"),
        "followed: {paths:?}"
    );
    assert!(
        paths.iter().all(|p| !p.contains("loop/")),
        "a loop is not descended: {paths:?}"
    );
}

// (k) `follow_symlinks = true` stays bounded and never reads out through a file link: a link to an
// ancestor of the root is not walked, and a file link is indexed only when it stays inside the root
// or inside a folder reached through an admitted directory link.
#[cfg(unix)]
#[test]
fn follow_symlinks_skips_ancestor_links_and_file_links_leaving_every_allowed_root() {
    use std::os::unix::fs::symlink;
    let outer = common::TempDir::new();
    let root = outer.path().join("root");
    fs::create_dir_all(&root).unwrap();
    fs::write(outer.path().join("sibling.txt"), "").unwrap();
    symlink(outer.path(), root.join("up")).unwrap();

    let data = common::TempDir::new();
    let secret = common::TempDir::new();
    fs::write(data.path().join("note.md"), "").unwrap();
    fs::write(secret.path().join("id"), "").unwrap();
    symlink(data.path(), root.join("data")).unwrap();
    symlink(data.path().join("note.md"), data.path().join("alias.md")).unwrap();
    symlink(secret.path().join("id"), data.path().join("leak")).unwrap();
    symlink(secret.path().join("id"), root.join("key")).unwrap();

    let paths = index::build_following(&root, true);
    for want in ["data/note.md", "data/alias.md"] {
        assert!(paths.iter().any(|p| p == want), "{want} indexed: {paths:?}");
    }
    for leak in ["key", "data/leak"] {
        assert!(
            !paths.iter().any(|p| p == leak),
            "{leak} not indexed: {paths:?}"
        );
    }
    assert!(
        paths.iter().all(|p| !p.starts_with("up/")),
        "no walk through a link to an ancestor: {paths:?}"
    );
}

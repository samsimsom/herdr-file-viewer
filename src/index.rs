//! File Index — a recursive, gitignore-aware walk that returns every file under `root`
//! as a root-relative path string.
//!
//! Used by the Go-to-file feature (AC-12…AC-15, AC-18, AC-19, AC-N1, AC-N2, AC-N5).
//! This is a separate walk from the Tree Model (ADR-0005): no depth limit, files only,
//! and the entire `.git` subtree is pruned via `filter_entry`.

use ignore::WalkBuilder;
use std::path::{Component, Path, PathBuf};

/// The shared base for the crate's two gitignore-aware walks — this File Index and the Tree
/// Model (`tree.rs`). Sets the hermetic policy both share so it lives in one place: honor an
/// ancestor `.gitignore`, ignore the user's global gitignore and generic `.ignore` files, and
/// apply `.gitignore` even outside a git repo. The caller sets what differs between the two
/// walks — depth, dotfile hiding, and whether `.gitignore`/`.git/info/exclude` are honored.
///
/// `is_git_repo` bounds how far the `parents(true)` ancestor search is allowed to climb.
/// `ignore::WalkBuilder`'s ancestor search always walks every parent directory up to the
/// filesystem root looking for `.gitignore` files (`parents(true)` has no "stop at the repo
/// root" option of its own) — `require_git` is the only knob that bounds it, by gating the
/// search on finding a `.git`/`.jj` directory. When `root` is itself a git repository, passing
/// `require_git(true)` makes that search stop exactly at `root`'s own `.git` (matching real
/// git's behavior: only ancestors *inside* the repository can ever affect it), so an unrelated
/// enclosing directory or repository above it — a dotfiles checkout, `$HOME`, anything with its
/// own `.gitignore` — can no longer reach in and hide the whole tree. Filed upstream after a
/// git repo nested under `~/stow/documents-ruben` (itself a git repo, with `~/.gitignore`
/// containing a bare `*`) rendered completely empty: `require_git(false)` let the ancestor climb
/// walk straight past the repo boundary into that unrelated checkout and up to `$HOME`.
/// When `root` is NOT a git repo, `require_git(false)` is kept (AC-19: a plain folder's own
/// `.gitignore` is still honored even with no `.git` anywhere in its ancestry).
pub(crate) fn walk_builder(root: &Path, is_git_repo: bool) -> WalkBuilder {
    let mut builder = WalkBuilder::new(root);
    builder
        .parents(true) // honor ancestor .gitignore for correct nested semantics
        .git_global(false) // hermetic: ignore the user's global gitignore
        .ignore(false) // only git ignore sources, not generic .ignore files
        // Bound the ancestor search at `root`'s own repo boundary when it has one; otherwise
        // fall back to honoring `.gitignore` even outside a repo (AC-13, AC-19, AC-4, AC-26).
        .require_git(is_git_repo);
    builder
}

/// The single symlink admission rule the tree, the finder index and the content reader share (#164).
///
/// `link` is a symlink at or below `root`, and `canon_root` is `root` canonicalized once by the
/// caller (the tree asks about many entries, on every frame).
///
/// - A link that resolves **inside** the canonical root is always admitted: the containment rule
///   `render::classify` and `git::is_within_root` already apply to content reads (AC-N5).
/// - A link that resolves outside it is admitted only with the `follow_symlinks` opt-in, and then:
///   - a **directory** link unless its target is the root or one of the root's ancestors (`..`,
///     `~`, `/`). Following one of those would list the root inside itself and hand the finder a
///     walk of the whole filesystem;
///   - a **file** link only when its target stays inside an *allowed root*: the canonical root, or
///     the canonical target of a directory link on the file's own path. So `data/alias.md ->
///     data/note.md` reads, while `key -> ~/.ssh/id_rsa` and `data/leak -> ~/.ssh/id_rsa` never do.
/// - A dangling or otherwise unresolvable link is never admitted.
///
/// The link itself lives under the root, so every listed path stays root-relative; only the bytes
/// behind an admitted link come from elsewhere.
pub(crate) fn admit_symlink(
    root: &Path,
    canon_root: &Path,
    link: &Path,
    follow_symlinks: bool,
) -> bool {
    let Ok(target) = link.canonicalize() else {
        return false;
    };
    if target.starts_with(canon_root) {
        return true;
    }
    if !follow_symlinks {
        return false;
    }
    if target.is_dir() {
        return !canon_root.starts_with(&target);
    }
    link.parent().is_some_and(|dir| {
        followed_dir_targets(root, dir)
            .iter()
            .any(|allowed| target.starts_with(allowed))
    })
}

/// The canonical targets of the directory symlinks on `dir`'s path below `root`, outermost first:
/// the extra allowed roots a file link inside `dir` may resolve into ([`admit_symlink`]).
fn followed_dir_targets(root: &Path, dir: &Path) -> Vec<PathBuf> {
    let Ok(rel) = dir.strip_prefix(root) else {
        return Vec::new();
    };
    let mut prefix = root.to_path_buf();
    let mut targets = Vec::new();
    for component in rel.components() {
        prefix.push(component);
        if prefix.is_symlink()
            && let Ok(target) = prefix.canonicalize()
        {
            targets.push(target);
        }
    }
    targets
}

/// Whether `path` lies lexically below `root` (only normal components, so no `..`) and every
/// symlink along it, outermost first, is admitted by [`admit_symlink`]. The content reader's check:
/// a path handed over by the tree, the finder or a launch open target reads only when neither walk
/// would have refused a link on the way to it.
pub(crate) fn every_symlink_admitted(
    root: &Path,
    canon_root: &Path,
    path: &Path,
    follow_symlinks: bool,
) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let mut prefix = root.to_path_buf();
    for component in rel.components() {
        if !matches!(component, Component::Normal(_)) {
            return false;
        }
        prefix.push(component);
        if prefix.is_symlink() && !admit_symlink(root, canon_root, &prefix, follow_symlinks) {
            return false;
        }
    }
    true
}

/// Return every file under `root` as a root-relative `String`, respecting `.gitignore`.
/// Equivalent to [`build_scoped`] with `is_git_repo = false` — kept for callers (and the
/// existing test suite) that don't have a resolved git-repo flag to pass.
///
/// - Recursive (no depth limit) — AC-12.
/// - `.gitignore`-d files are excluded — AC-13.
/// - The `.git` subtree is pruned entirely — AC-14.
/// - Directories are not included, only files — AC-15.
/// - Every returned path is relative to `root` (no leading `/`, no `..`) — AC-N5.
/// - Symlinks are followed only when they resolve inside `root` ([`admit_symlink`]); the walker's
///   own loop detection drops a link back to an ancestor, so the walk stays bounded.
/// - Each call performs a fresh walk; no cache — AC-18.
/// - Works in non-git directories without error (`require_git(false)`) — AC-19.
/// - Read-only: no filesystem or git mutations — AC-N1, AC-N2.
pub fn build(root: &Path) -> Vec<String> {
    build_scoped_following(root, false, false)
}

/// Like [`build`], but bounds the ancestor `.gitignore` search at `root`'s own repository
/// boundary when `is_git_repo` is true (see [`walk_builder`]) instead of letting it climb past
/// an unrelated enclosing directory/repository above `root`.
pub fn build_scoped(root: &Path, is_git_repo: bool) -> Vec<String> {
    build_scoped_following(root, is_git_repo, false)
}

/// [`build`], with the `follow_symlinks` opt-in: when `true`, a directory link under `root` is
/// followed out of the root unless it points back up at the root or an ancestor, and a file link
/// only when it stays inside an allowed root ([`admit_symlink`]). Paths are reported under the link.
pub fn build_following(root: &Path, follow_symlinks: bool) -> Vec<String> {
    build_scoped_following(root, false, follow_symlinks)
}

/// [`build_scoped`] and [`build_following`] in one: the repo-boundary flag and the
/// `follow_symlinks` opt-in are independent, and the finder passes both.
pub fn build_scoped_following(
    root: &Path,
    is_git_repo: bool,
    follow_symlinks: bool,
) -> Vec<String> {
    let mut builder = walk_builder(root, is_git_repo);
    let bound = root.to_path_buf();
    let canon_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    builder
        .hidden(false) // include dotfiles (AC-17 depends on the index NOT hiding dotfiles)
        .git_ignore(true)
        .git_exclude(true)
        .follow_links(true) // #164: a symlinked directory's files are findable…
        .filter_entry(move |e| {
            e.file_name() != ".git" // prune entire .git subtree — AC-14
                // …but only links the shared rule admits (AC-N5 unless opted in, #164).
                && (!e.path_is_symlink()
                    || admit_symlink(&bound, &canon_root, e.path(), follow_symlinks))
        });

    builder
        .build()
        .filter_map(Result::ok) // skip unreadable entries; traversal continues
        .filter(|e| e.file_type().is_some_and(|t| t.is_file())) // files only — AC-15
        .filter_map(|e| e.path().strip_prefix(root).ok().map(rel_to_slash))
        .collect()
}

/// Render a root-relative path as a forward-slash string on every platform. The rest of the app
/// (git status/diff/worktree paths, the tree, the content title) speaks git's forward-slash
/// convention; on Windows the native separator is `\`, so a raw stringification would make the
/// finder's listing inconsistent with the rest of the UI. Joining the path's `Normal` components
/// with `/` is identical to today's output on unix (the separator already is `/`) and converts
/// `a\b` → `a/b` on Windows. It also enforces AC-N5 (root-relative, no `..`/absolute leak) by
/// construction.
fn rel_to_slash(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

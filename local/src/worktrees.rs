//! Resolve PR checkouts and open them in VS Code. Git runs off the UI thread.
//! Creation follows tools/wt.sh: <repo>.worktrees/<slug(branch)>, with no
//! overwriting of an existing branch or path.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const MAX_ENTRIES: usize = 512;
const MAX_DIRS: usize = 100_000;
const MAX_CACHE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone)]
pub struct PullRequest {
    pub repository: String,
    pub head_repository: String,
    pub branch: String,
    pub number: u64,
}

/// A cached path counts only while it is inside one of the folders searched
/// now, so changing the folders forgets just the entries they no longer cover.
#[derive(Default, Serialize, Deserialize)]
struct Cache {
    repositories: Vec<(String, PathBuf)>,
    pulls: Vec<(String, PathBuf)>,
}

fn remember(entries: &mut Vec<(String, PathBuf)>, key: String, path: PathBuf) {
    entries.retain(|(k, _)| k != &key);
    entries.push((key, path));
    if entries.len() > MAX_ENTRIES {
        entries.remove(0);
    }
}

/// SSH and HTTPS spellings share a host/owner/repository identity.
pub fn remote_identity(remote: &str) -> Option<String> {
    let remote = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    let (host, path) = if let Some((_, rest)) = remote.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        (authority.rsplit('@').next()?, path)
    } else {
        let (authority, path) = remote.split_once(':')?;
        (authority.rsplit('@').next()?, path)
    };
    let parts: Vec<_> = path.split('/').collect();
    if host.is_empty()
        || parts.len() != 2
        || parts
            .iter()
            .any(|s| s.is_empty() || *s == "." || *s == "..")
    {
        return None;
    }
    Some(format!("{host}/{path}").to_ascii_lowercase())
}

/// Exactly the ASCII slug rules used by wt.sh.
fn slugify(branch: &str) -> String {
    let mut slug = String::new();
    for ch in branch.to_ascii_lowercase().chars() {
        let ch = if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            ch
        } else {
            '-'
        };
        if ch != '-' || (!slug.is_empty() && !slug.ends_with('-')) {
            slug.push(ch);
        }
    }
    slug.trim_end_matches('-').to_owned()
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args);
    let output = prmarmot_core::process::run_bounded(
        &mut command,
        Duration::from_secs(60),
        1024 * 1024,
        true,
    )
    .map_err(|e| format!("Could not run Git: {e}"))?
    .ok_or("Git timed out after 60 seconds")?;
    if !output.status.success() {
        return Err(format!(
            "Git {} failed: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if output.stdout.len() >= 1024 * 1024 {
        return Err("Git output exceeded the size limit".into());
    }
    String::from_utf8(output.stdout).map_err(|_| "Git returned a path that is not UTF-8".into())
}

fn matching_remote(repo: &Path, identity: &str) -> Option<String> {
    git(repo, &["remote"]).ok()?.lines().find_map(|name| {
        let url = git(repo, &["config", "--get", &format!("remote.{name}.url")]).ok()?;
        (remote_identity(&url).as_deref() == Some(identity)).then(|| name.to_owned())
    })
}

/// Search the folders in order; the first clone with a matching remote wins.
/// One limit covers the whole search, and a folder inside an earlier one is
/// not searched twice. An unreadable folder is skipped unless all are.
fn discover(roots: &[PathBuf], identity: &str) -> Result<PathBuf, String> {
    let started = Instant::now();
    let mut visited = 0;
    let mut unreadable = None;
    let mut readable = false;
    for (index, root) in roots.iter().enumerate() {
        let earlier = &roots[..index];
        if earlier.iter().any(|done| root.starts_with(done)) {
            continue;
        }
        let mut pending = vec![root.clone()];
        while let Some(path) = pending.pop() {
            visited += 1;
            if visited > MAX_DIRS || started.elapsed() > Duration::from_secs(60) {
                return Err(
                    "Repository search limit reached; choose narrower Repositories folders".into(),
                );
            }
            if path.join(".git").exists() && matching_remote(&path, identity).is_some() {
                return Ok(path);
            }
            let entries = match std::fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(e) if &path == root => {
                    unreadable.get_or_insert(format!("Cannot read repositories folder: {e}"));
                    continue;
                }
                Err(_) => continue,
            };
            readable = true;
            let mut dirs: Vec<_> = entries
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_name() != ".git" && entry.file_type().is_ok_and(|t| t.is_dir())
                })
                .map(|entry| entry.path())
                .filter(|path| !earlier.contains(path))
                .collect();
            dirs.sort();
            pending.extend(dirs.into_iter().rev());
        }
    }
    match unreadable {
        Some(error) if !readable => Err(error),
        _ => Err(format!(
            "No repository matching {identity} found in {}",
            roots
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[derive(Debug)]
struct Worktree {
    path: PathBuf,
    branch: Option<String>,
    unavailable: bool,
}

fn worktrees(repo: &Path) -> Result<Vec<Worktree>, String> {
    let listing = git(repo, &["worktree", "list", "--porcelain", "-z"])?;
    let mut trees: Vec<Worktree> = Vec::new();
    for field in listing.split('\0') {
        if let Some(path) = field.strip_prefix("worktree ") {
            trees.push(Worktree {
                path: path.into(),
                branch: None,
                unavailable: false,
            });
        } else if let Some(tree) = trees.last_mut() {
            if let Some(branch) = field.strip_prefix("branch refs/heads/") {
                tree.branch = Some(branch.into());
            }
            if field.starts_with("prunable") {
                tree.unavailable = true;
            }
        }
    }
    Ok(trees)
}

fn matches_pr(repo: &Path, tree: &Worktree, pr: &PullRequest) -> bool {
    if tree.unavailable || !tree.path.is_dir() {
        return false;
    }
    let Some(branch) = tree.branch.as_deref() else {
        return false;
    };
    // Upstream identity also supports differently named local tracking branches.
    let remote = git(
        repo,
        &["config", "--get", &format!("branch.{branch}.remote")],
    )
    .ok();
    let merge = git(
        repo,
        &["config", "--get", &format!("branch.{branch}.merge")],
    )
    .ok();
    match (remote, merge) {
        (Some(remote), Some(merge)) => {
            let remote = remote.trim();
            let url = git(repo, &["config", "--get", &format!("remote.{remote}.url")])
                .unwrap_or_else(|_| remote.to_owned());
            remote_identity(&url).as_deref() == Some(pr.head_repository.as_str())
                && merge.trim() == format!("refs/heads/{}", pr.branch)
        }
        (None, None) => branch == pr.branch && pr.head_repository == pr.repository,
        _ => false,
    }
}

/// Resolve first, then persist, so editor failures never create extra worktrees.
pub fn resolve(roots: &[PathBuf], cache_path: &Path, pr: &PullRequest) -> Result<PathBuf, String> {
    resolve_inner(roots, cache_path, pr, false)
}

/// Find and cache the repository without fetching or creating a checkout.
/// Agents uses this repository as the parent of its own managed worktree.
pub fn resolve_repository(
    roots: &[PathBuf],
    cache_path: &Path,
    pr: &PullRequest,
) -> Result<PathBuf, String> {
    resolve_inner(roots, cache_path, pr, true)
}

fn resolve_inner(
    roots: &[PathBuf],
    cache_path: &Path,
    pr: &PullRequest,
    repository_only: bool,
) -> Result<PathBuf, String> {
    if roots.is_empty() || roots.iter().any(|root| !root.is_absolute()) {
        return Err("Set absolute Repositories folders in Settings".into());
    }
    // A folder that is missing (an unplugged drive, say) is left out, so the
    // others still work; only when none can be opened is that the error.
    let mut missing = None;
    let roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| {
            root.canonicalize()
                .map_err(|e| {
                    missing.get_or_insert(format!("Cannot open repositories folder: {e}"));
                })
                .ok()
        })
        .collect();
    if roots.is_empty() {
        return Err(missing.unwrap_or_default());
    }
    let mut cache: Cache = std::fs::metadata(cache_path)
        .ok()
        .filter(|m| m.len() <= MAX_CACHE_BYTES)
        .and_then(|_| std::fs::read(cache_path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    if cache.repositories.len() > MAX_ENTRIES || cache.pulls.len() > MAX_ENTRIES {
        cache = Cache::default();
    }
    let repo = cache
        .repositories
        .iter()
        .find(|(key, path)| {
            key == &pr.repository
                && path
                    .canonicalize()
                    .is_ok_and(|p| roots.iter().any(|root| p.starts_with(root)))
                && matching_remote(path, &pr.repository).is_some()
        })
        .map(|(_, path)| path.clone());
    let repo = match repo {
        Some(repo) => repo,
        None => discover(&roots, &pr.repository)?,
    };
    remember(&mut cache.repositories, pr.repository.clone(), repo.clone());
    save_cache(cache_path, &cache)?;
    if repository_only {
        return Ok(repo);
    }
    git(&repo, &["check-ref-format", "--branch", &pr.branch])?;
    if pr.branch.starts_with('-') || pr.branch.starts_with('@') {
        return Err("Unsupported PR branch name".into());
    }
    let key = format!("{}/{}", pr.repository, pr.number);
    let trees = worktrees(&repo)?;
    let cached = cache
        .pulls
        .iter()
        .find(|(k, _)| k == &key)
        .and_then(|(_, path)| {
            trees
                .iter()
                .find(|tree| &tree.path == path && matches_pr(&repo, tree, pr))
        });
    let existing = cached.or_else(|| trees.iter().find(|tree| matches_pr(&repo, tree, pr)));
    let path = if let Some(tree) = existing {
        tree.path.clone()
    } else {
        create(&repo, pr)?
    };
    remember(&mut cache.pulls, key, path.clone());
    save_cache(cache_path, &cache)?;
    Ok(path)
}

fn save_cache(path: &Path, cache: &Cache) -> Result<(), String> {
    let bytes = serde_json::to_vec(cache).map_err(|e| e.to_string())?;
    crate::attention_state::atomic_write(path, &bytes)
        .map_err(|e| format!("Could not save checkout cache: {e}"))
}

fn create(repo: &Path, pr: &PullRequest) -> Result<PathBuf, String> {
    create_branch(repo, pr, &pr.branch, true)
}

fn create_branch(
    repo: &Path,
    pr: &PullRequest,
    branch: &str,
    track: bool,
) -> Result<PathBuf, String> {
    let branch_ref = format!("refs/heads/{}", branch);
    if git(repo, &["show-ref", "--verify", "--quiet", &branch_ref]).is_ok() {
        return Err(format!(
            "Branch already exists: {}. Open it in a worktree first.",
            branch
        ));
    }
    let slug = slugify(branch);
    if slug.is_empty() || slug == "." || slug == ".." {
        return Err("Branch has no usable worktree folder name".into());
    }
    let top = git(repo, &["rev-parse", "--show-toplevel"])?;
    let parent = PathBuf::from(format!("{}.worktrees", top.trim_end_matches('\n')));
    let path = parent.join(slug);
    if path.symlink_metadata().is_ok() {
        return Err(format!("Worktree path already exists: {}", path.display()));
    }
    let remote = matching_remote(repo, &pr.repository)
        .ok_or("Repository remote no longer matches the PR")?;
    // GitHub exposes fork PR heads on the base repository too.
    let fetched_ref = format!("refs/prmarmot/pull/{}", pr.number);
    git(
        repo,
        &[
            "fetch",
            "--no-tags",
            "--",
            &remote,
            &format!("+refs/pull/{}/head:{fetched_ref}", pr.number),
        ],
    )?;
    let oid = git(
        repo,
        &[
            "rev-parse",
            "--verify",
            &format!("{fetched_ref}^{{commit}}"),
        ],
    )?;
    std::fs::create_dir_all(&parent)
        .map_err(|e| format!("Could not create worktrees folder: {e}"))?;
    let path_str = path.to_str().ok_or("Worktree path is not UTF-8")?;
    git(
        repo,
        &["worktree", "add", "-b", branch, "--", path_str, oid.trim()],
    )?;
    if !track {
        return Ok(path);
    }
    // Record the actual head repository, never the base repo's synthetic PR ref.
    let upstream = matching_remote(repo, &pr.head_repository)
        .unwrap_or_else(|| format!("https://{}.git", pr.head_repository));
    git(
        repo,
        &["config", &format!("branch.{}.remote", branch), &upstream],
    )
    .map_err(|e| {
        format!(
            "Created {}, but could not record upstream: {e}",
            path.display()
        )
    })?;
    git(
        repo,
        &["config", &format!("branch.{}.merge", branch), &branch_ref],
    )?;
    Ok(path)
}

pub fn open_vscode(path: &Path) -> Result<(), String> {
    let mut command = Command::new("code");
    command.arg("-n").arg(path);
    let result =
        prmarmot_core::process::run_bounded(&mut command, Duration::from_secs(15), 16 * 1024, true);
    #[cfg(target_os = "macos")]
    let result = if result
        .as_ref()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
    {
        let mut command = Command::new("/usr/bin/open");
        command
            .args(["-n", "-a", "Visual Studio Code", "--args", "-n"])
            .arg(path);
        prmarmot_core::process::run_bounded(&mut command, Duration::from_secs(15), 16 * 1024, true)
    } else {
        result
    };
    let output = result
        .map_err(|e| format!("Could not launch VS Code; install its code command: {e}"))?
        .ok_or("VS Code launch timed out")?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Could not open VS Code: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: PathBuf,
        repo: PathBuf,
        cache: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "prmarmot-wt-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let root = {
                std::fs::create_dir_all(&root).unwrap();
                root.canonicalize().unwrap()
            };
            let repo = root.join("nested/repo");
            std::fs::create_dir_all(&repo).unwrap();
            git(&repo, &["init", "-b", "main"]).unwrap();
            git(&repo, &["config", "core.hooksPath", "/dev/null"]).unwrap();
            git(
                &repo,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "initial",
                ],
            )
            .unwrap();
            git(
                &repo,
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://github.test/acme/widgets.git",
                ],
            )
            .unwrap();
            // Fetches stay entirely local, while configured remote identity is GitHub.
            let source = root.join("source.git");
            git(
                &root,
                &[
                    "clone",
                    "--bare",
                    repo.to_str().unwrap(),
                    source.to_str().unwrap(),
                ],
            )
            .unwrap();
            let pr_commit = git(
                &source,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "-c",
                    "commit.gpgsign=false",
                    "commit-tree",
                    "HEAD^{tree}",
                    "-p",
                    "HEAD",
                    "-m",
                    "PR head",
                ],
            )
            .unwrap();
            git(
                &source,
                &["update-ref", "refs/pull/42/head", pr_commit.trim()],
            )
            .unwrap();
            git(
                &repo,
                &[
                    "config",
                    &format!("url.{}.insteadOf", source.display()),
                    "https://github.test/acme/widgets.git",
                ],
            )
            .unwrap();
            git(&repo, &["config", "protocol.file.allow", "always"]).unwrap();
            Self {
                cache: root.join("cache/worktrees.json"),
                root,
                repo,
            }
        }
        fn pr(&self) -> PullRequest {
            PullRequest {
                repository: "github.test/acme/widgets".into(),
                head_repository: "github.test/acme/widgets".into(),
                branch: "Feature/IFR-123_Test".into(),
                number: 42,
            }
        }
        fn roots(&self) -> Vec<PathBuf> {
            vec![self.root.clone()]
        }
        fn resolve(&self, pr: &PullRequest) -> Result<PathBuf, String> {
            resolve(&self.roots(), &self.cache, pr)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn remote_spellings_and_hosts() {
        for url in [
            "git@github.com:Acme/Widgets.git",
            "ssh://git@github.com/Acme/Widgets.git",
            "https://github.com/Acme/Widgets.git/",
        ] {
            assert_eq!(
                remote_identity(url).as_deref(),
                Some("github.com/acme/widgets")
            );
        }
        assert_ne!(
            remote_identity("https://enterprise.test/acme/widgets"),
            remote_identity("https://github.com/acme/widgets")
        );
        assert!(remote_identity("/tmp/a").is_none());
    }

    #[test]
    fn wt_script_slug_rules() {
        assert_eq!(slugify("Feature/IFR-123__Test"), "feature-ifr-123__test");
        assert_eq!(slugify("--A///B---C.._--"), "a-b-c.._");
        assert_eq!(slugify("☃/"), "");
    }

    #[test]
    fn creates_and_reuses_worktree_with_persistent_cache_and_dirty_files() {
        let f = Fixture::new();
        let pr = f.pr();
        let path = f.resolve(&pr).unwrap();
        assert_eq!(
            path,
            PathBuf::from(format!(
                "{}.worktrees/feature-ifr-123_test",
                f.repo.display()
            ))
        );
        assert_eq!(
            git(&path, &["branch", "--show-current"]).unwrap().trim(),
            pr.branch
        );
        assert_ne!(
            git(&path, &["rev-parse", "HEAD"]).unwrap(),
            git(&f.repo, &["rev-parse", "HEAD"]).unwrap()
        );
        assert_eq!(
            git(&path, &["log", "-1", "--format=%s"]).unwrap().trim(),
            "PR head"
        );
        std::fs::write(path.join("keep.txt"), "uncommitted").unwrap();
        assert_eq!(f.resolve(&pr).unwrap(), path);
        assert_eq!(
            std::fs::read_to_string(path.join("keep.txt")).unwrap(),
            "uncommitted"
        );
        let cache: Cache = serde_json::from_slice(&std::fs::read(&f.cache).unwrap()).unwrap();
        assert_eq!(cache.repositories.len(), 1);
        assert_eq!(
            cache.pulls,
            vec![("github.test/acme/widgets/42".into(), path)]
        );
    }

    #[test]
    fn discovers_existing_main_checkout_and_stale_cache_recovers() {
        let f = Fixture::new();
        let mut pr = f.pr();
        pr.branch = "main".into();
        assert_eq!(f.resolve(&pr).unwrap(), f.repo);
        let mut cache: Cache = serde_json::from_slice(&std::fs::read(&f.cache).unwrap()).unwrap();
        cache.repositories[0].1 = f.root.join("gone");
        cache.pulls[0].1 = f.root.join("gone");
        save_cache(&f.cache, &cache).unwrap();
        assert_eq!(f.resolve(&pr).unwrap(), f.repo);
        std::fs::write(&f.cache, "invalid json").unwrap();
        assert_eq!(f.resolve(&pr).unwrap(), f.repo);
    }

    #[test]
    fn refuses_existing_branch_and_slug_path_collision() {
        let f = Fixture::new();
        let pr = f.pr();
        git(&f.repo, &["branch", &pr.branch]).unwrap();
        assert!(f
            .resolve(&pr)
            .unwrap_err()
            .contains("Branch already exists"));
        git(&f.repo, &["branch", "-D", &pr.branch]).unwrap();
        let path = PathBuf::from(format!(
            "{}.worktrees/{}",
            f.repo.display(),
            slugify(&pr.branch)
        ));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("keep"), "mine").unwrap();
        assert!(f
            .resolve(&pr)
            .unwrap_err()
            .contains("Worktree path already exists"));
        assert_eq!(std::fs::read_to_string(path.join("keep")).unwrap(), "mine");
    }

    #[test]
    fn fork_identity_is_checked_even_on_cache_hit() {
        let f = Fixture::new();
        let mut pr = f.pr();
        pr.head_repository = "github.test/contributor/widgets".into();
        let path = f.resolve(&pr).unwrap();
        assert_eq!(f.resolve(&pr).unwrap(), path);
        pr.head_repository = "github.test/other/widgets".into();
        assert!(f
            .resolve(&pr)
            .unwrap_err()
            .contains("Branch already exists"));
    }

    #[test]
    fn agents_repository_resolution_creates_no_checkout() {
        let f = Fixture::new();
        let pr = f.pr();
        std::fs::write(f.repo.join("unfinished.txt"), "keep me").unwrap();
        let before = git(&f.repo, &["worktree", "list", "--porcelain"]).unwrap();
        assert_eq!(
            resolve_repository(&f.roots(), &f.cache, &pr).unwrap(),
            f.repo
        );
        assert_eq!(
            git(&f.repo, &["worktree", "list", "--porcelain"]).unwrap(),
            before
        );
        assert_eq!(
            std::fs::read_to_string(f.repo.join("unfinished.txt")).unwrap(),
            "keep me"
        );
        assert!(git(&f.repo, &["rev-parse", "--verify", "refs/prmarmot/pull/42"]).is_err());
    }

    #[test]
    fn reuses_differently_named_tracking_branch() {
        let f = Fixture::new();
        let mut pr = f.pr();
        pr.head_repository = "github.test/contributor/widgets".into();
        let path = f.resolve(&pr).unwrap();
        git(&path, &["branch", "-m", "my-review"]).unwrap();
        assert_eq!(f.resolve(&pr).unwrap(), path);
    }

    #[test]
    fn removed_worktree_does_not_reuse_cached_path_or_reset_retained_branch() {
        let f = Fixture::new();
        let pr = f.pr();
        let path = f.resolve(&pr).unwrap();
        git(&f.repo, &["worktree", "remove", path.to_str().unwrap()]).unwrap();
        assert!(f
            .resolve(&pr)
            .unwrap_err()
            .contains("Branch already exists"));
    }

    #[test]
    fn a_cached_repository_outside_every_folder_is_found_again() {
        let a = Fixture::new();
        let b = Fixture::new();
        let mut pr = a.pr();
        pr.branch = "main".into();
        assert_eq!(a.resolve(&pr).unwrap(), a.repo);
        assert_eq!(resolve(&b.roots(), &a.cache, &pr).unwrap(), b.repo);
    }

    #[test]
    fn folders_are_searched_in_order_and_a_cached_pick_survives_reordering() {
        let a = Fixture::new();
        let b = Fixture::new();
        let mut pr = a.pr();
        pr.branch = "main".into();
        let both = vec![a.root.clone(), b.root.clone()];
        assert_eq!(resolve(&both, &a.cache, &pr).unwrap(), a.repo);
        let reversed = vec![b.root.clone(), a.root.clone()];
        assert_eq!(
            resolve(&reversed, &a.cache, &pr).unwrap(),
            a.repo,
            "the clone already in use stays in use"
        );
        std::fs::remove_file(&a.cache).unwrap();
        assert_eq!(resolve(&reversed, &a.cache, &pr).unwrap(), b.repo);
    }

    #[test]
    fn a_missing_folder_is_skipped_unless_every_folder_is_missing() {
        let f = Fixture::new();
        let mut pr = f.pr();
        pr.branch = "main".into();
        let gone = f.root.join("unplugged");
        let roots = vec![gone.clone(), f.root.clone()];
        assert_eq!(resolve(&roots, &f.cache, &pr).unwrap(), f.repo);
        assert!(resolve(&[gone], &f.cache, &pr)
            .unwrap_err()
            .contains("Cannot open repositories folder"));
    }

    #[test]
    fn a_folder_inside_another_is_searched_once_and_keeps_its_place() {
        let f = Fixture::new();
        let first = f.root.join("a-first");
        git(
            &f.root,
            &["clone", f.repo.to_str().unwrap(), first.to_str().unwrap()],
        )
        .unwrap();
        git(
            &first,
            &[
                "remote",
                "set-url",
                "origin",
                "git@github.test:acme/widgets.git",
            ],
        )
        .unwrap();
        let nested = f.root.join("nested");
        let roots = vec![nested.clone(), f.root.clone()];
        assert_eq!(
            discover(&roots, "github.test/acme/widgets").unwrap(),
            f.repo,
            "the first folder wins although the second sorts a clone earlier"
        );
        assert!(
            discover(&[nested, f.root.clone()], "github.test/other/repo")
                .unwrap_err()
                .contains(", ")
        );
    }

    #[test]
    fn scan_order_is_stable_and_does_not_follow_symlinks() {
        let f = Fixture::new();
        let first = f.root.join("a-first");
        git(
            &f.root,
            &["clone", f.repo.to_str().unwrap(), first.to_str().unwrap()],
        )
        .unwrap();
        git(
            &first,
            &[
                "remote",
                "set-url",
                "origin",
                "git@github.test:acme/widgets.git",
            ],
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&f.root, f.root.join("0-loop")).unwrap();
        assert_eq!(
            discover(&f.roots(), "github.test/acme/widgets").unwrap(),
            first
        );
    }

    #[test]
    fn remote_change_invalidates_cached_repository_and_missing_pr_creates_nothing() {
        let f = Fixture::new();
        let mut pr = f.pr();
        pr.number = 404;
        assert!(f.resolve(&pr).unwrap_err().contains("fetch failed"));
        assert!(git(
            &f.repo,
            &["show-ref", "--verify", &format!("refs/heads/{}", pr.branch)]
        )
        .is_err());
        git(
            &f.repo,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.test/other/repo.git",
            ],
        )
        .unwrap();
        assert!(f
            .resolve(&pr)
            .unwrap_err()
            .contains("No repository matching"));
    }

    #[test]
    fn caches_evict_old_entries() {
        let mut entries = Vec::new();
        for n in 0..=MAX_ENTRIES {
            remember(&mut entries, n.to_string(), PathBuf::from("/repo"));
        }
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(entries[0].0, "1");
    }
}

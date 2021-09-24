#[macro_use]
extern crate anyhow;
extern crate argh;
extern crate colog;
extern crate dashmap;
extern crate git2;
#[macro_use]
extern crate log;
extern crate markup;
extern crate num_cpus;
extern crate rayon;
extern crate syntect;
extern crate thread_local;

use anyhow::Result;
use argh::{FromArgValue, FromArgs};
use dashmap::{DashMap, DashSet};
use git2::{ObjectType, Oid, Repository, TreeEntry, TreeWalkMode, TreeWalkResult};
use rayon::prelude::*;
use std::fs::{create_dir_all, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use syntect::highlighting::ThemeSet;
use syntect::html::{css_for_theme_with_class_style, ClassStyle, ClassedHTMLGenerator};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use thread_local::ThreadLocal;

// matches sr.ht, one longer than GitHub/Gitlab
const PRETTY_OID_CHAR_LENGTH: usize = 8;

markup::define! {
    RenderedObject(lines: Vec<String>) {
        table {
            @for (idx, line) in lines.iter().enumerate() {
                tr#{format!("L{}", idx+1)} {
                    td."gawsh-line-number" {
                        a[href=format!("#L{}", idx+1)] {
                            pre { @format!("{}", idx+1) }
                        }
                    }
                    td."gawsh-line-content-wrapper" {
                        pre."gawsh-line-content" {
                            @markup::raw(line)
                        }
                    }
                }
            }
        }
    }

    TreeView<'a>(
        tree_oid: &'a Oid,
        tree_alias: &'a Option<&'a str>,
        objects: &'a Vec<Option<RenderableTreeObject>>,
    ) {
        div."gawsh-tree-header" {
            @if let Some(alias) = tree_alias {
                span."gawsh-tree-header-alias" { @alias }
                span."gawsh-tree-header-aliased-commitish" {
                    @format!("({})", pretty_oid(tree_oid))
                }
            } else {
                span."gawsh-tree-header-unaliased-commitish" {
                    @pretty_oid(tree_oid).to_string()
                }
            }
        }
        table."gawsh-tree-contents" {
            @for obj in objects.iter() {
                @if let Some(obj) = obj {
                    tr."gawsh-tree-line" {
                        td."gawsh-tree-line-name" {
                            a[href=obj.link()] {
                                @obj.name()
                            }
                        }
                    }
                }
            }
        }
    }
}

/// gawsh generates a static HTML portrait of a Git repository
// TODO?
//
// no-highlight: seems obvious. would be implied in a world where gawsh was built without the
// syntect feature (assuming it were made modular at some point). rationale: hella faster, and some
// folks just don't want highlighted files.
//
// no-history-links: don't generate links to past commits; only allow linking to objects referenced
// within the same commit tree. rationale: this, combined with --depth=1, allows for fast(er),
// cheap(er) rendering of the tip of a branch, but none of its ancestors (which are extremely
// expensive), and (attempts to) ensure no broken links are generated (for example, "commit
// abcde123 - parent: 321edcba" would _not_ hyperlink 321edcba with this flag, whereas the default
// would, as it would assume it has or will generate(d) the tree portrait for that commit
#[derive(FromArgs, PartialEq, Debug)]
struct CmdArgs {
    /// be chatty
    #[argh(switch, short = 'v')]
    verbose: bool,

    /// limit history walk depth to N commits. defaults to 0, meaning no limit (recurse from HEAD
    /// to the beginning of discoverable history)
    #[argh(option, short = 'd', default = "0")]
    depth: usize,

    /// maximum number of parallel jobs, defaults to number of CPU cores. bigger numbers are not
    /// always better, depending on the speed of your drives, amount of RAM, etc.
    #[argh(
        option,
        short = 'j', // this matches make, cargo, and countless others
        default = "num_cpus::get()"
    )]
    jobs: usize,

    /// repository to operate on, defaults to current directory
    #[argh(
        option,
        short = 'C', // this matches git and most community tools
        default = "String::from(\".\")"
    )]
    repository: String,

    /// output directory for rendered files, will be created if it doesn't exist. defaults to
    /// ./.gawsh-output
    #[argh(option, short = 'o', default = "String::from(\".gawsh-output\")")]
    output: String,

    /// templating behavior for embedding rendered Objects into tree files, must be one of
    /// <disabled|caddy>. defaults to disabled, the only behavior that is guaranteed to work no
    /// matter the destination.
    #[argh(option, default = "TemplatingBehavior::Disabled")]
    templating_behavior: TemplatingBehavior,

    /// behavior for representing entirely-duplicated files on disk, must be one of
    /// <copy|hardlink|symlink>. defaults to copy, the only behavior that is guaranteed to work no
    /// matter the destination.
    #[argh(option, short = 'l', default = "DuplicateLinkageBehavior::Copy")]
    #[cfg(unix)]
    duplicate_linkage_behavior: DuplicateLinkageBehavior,

    /// behavior for representing entirely-duplicated files on disk, must be one of
    /// <copy|hardlink>. defaults to copy, the only behavior that is guaranteed to work no
    /// matter the destination.
    #[argh(option, short = 'l', default = "DuplicateLinkageBehavior::Copy")]
    #[cfg(not(unix))]
    duplicate_linkage_behavior: DuplicateLinkageBehavior,
}

/// To save disk space, gawsh can render Objects (the files stored in the Git repository) to
/// snippet files which can then be referenced in an "include" statement from all pages that need
/// to show that version of the file in question. These statements inherently vary by templating
/// engine.
///
/// This functionality is disabled by default; all objects will simply be inline concatenated into
/// referring files no matter the disk space cost, as this is the only guaranteed-safe-everywhere
/// behavior that doesn't take runtime dependencies.
///
/// TODO determine if this is just bloat
#[derive(PartialEq, Debug)]
enum TemplatingBehavior {
    Disabled,
    Caddy,
}

impl FromArgValue for TemplatingBehavior {
    fn from_arg_value(val: &str) -> core::result::Result<Self, String> {
        match val.to_lowercase().as_str() {
            "disabled" => Ok(TemplatingBehavior::Disabled),
            "caddy" => Ok(TemplatingBehavior::Caddy),
            other => Err(format!(
                "unknown TemplatingBehavior {}, try disabled|caddy",
                other
            )),
        }
    }
}

#[derive(PartialEq, Debug)]
enum DuplicateLinkageBehavior {
    Copy,
    HardLink,
    #[cfg(unix)]
    SymLink,
}

impl FromArgValue for DuplicateLinkageBehavior {
    fn from_arg_value(val: &str) -> core::result::Result<Self, String> {
        match val.to_lowercase().as_str() {
            "copy" => Ok(DuplicateLinkageBehavior::Copy),
            "hardlink" => Ok(DuplicateLinkageBehavior::HardLink),
            #[cfg(unix)]
            "symlink" => Ok(DuplicateLinkageBehavior::SymLink),
            #[cfg(unix)]
            other => Err(format!(
                "unknown DuplicateLinkageBehavior {}, try copy|hardlink|symlink",
                other
            )),
            #[cfg(not(unix))]
            other => Err(format!(
                "unknown DuplicateLinkageBehavior {}, try copy|hardlink",
                other
            )),
        }
    }
}

type InternedFilenamesByOid = DashMap<Oid, usize>;
type InternedFilenames = DashMap<usize, String>;

#[derive(Debug)]
struct ReferencedOids {
    oids: InternedFilenamesByOid,
    filenames: InternedFilenames,
}

type SerializedOid = Vec<u8>;
type SerializedOids = Vec<SerializedOid>;

#[derive(Debug, Eq, Hash, PartialEq)]
// this has to be pub to make markup.rs happy
//
// these should probably be interned somewhere much as filenames are
pub enum RenderableTreeObject {
    Tree { oid: Oid, name: String },
    TextFile { oid: Oid, name: String },
    BinaryFile { oid: Oid, name: String },
}

impl RenderableTreeObject {
    // I give up trying to fix this lint
    #[allow(clippy::needless_arbitrary_self_type)]
    pub fn link(self: &Self) -> String {
        match self {
            // TODO FIXME
            RenderableTreeObject::Tree { oid, .. } => format!("../tree/{}.html", oid),
            RenderableTreeObject::TextFile { oid, .. }
            | RenderableTreeObject::BinaryFile { oid, .. } => format!("../oid/{}.html", oid),
        }
    }

    // I give up trying to fix this lint
    #[allow(clippy::needless_arbitrary_self_type)]
    pub fn name(self: &Self) -> &str {
        match self {
            RenderableTreeObject::Tree { name, .. }
            | RenderableTreeObject::TextFile { name, .. }
            | RenderableTreeObject::BinaryFile { name, .. } => name,
        }
    }
}

fn pretty_oid(oid: &Oid) -> String {
    oid.to_string()
        .chars()
        .take(PRETTY_OID_CHAR_LENGTH)
        .collect()
}

fn main() -> Result<()> {
    let args: CmdArgs = argh::from_env();

    let mut clog = colog::builder();
    clog.filter(
        None,
        if args.verbose {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        },
    );
    clog.init();

    rayon::ThreadPoolBuilder::new()
        .num_threads(args.jobs)
        .build_global()?;

    let output_root = PathBuf::from(&args.output);
    let oid_target = {
        let mut target = output_root.clone();
        target.push("oid");
        Arc::new(target)
    };
    let tree_target = {
        let mut target = output_root.clone();
        target.push("tree");
        Arc::new(target)
    };
    let ref_target = {
        let mut target = output_root.clone();
        target.push("ref");
        Arc::new(target)
    };
    drop(output_root);
    create_dir_all(&*oid_target)?;
    create_dir_all(&*tree_target)?;
    create_dir_all(&*ref_target)?;

    let repo = Repository::open(&args.repository)?;
    let head = repo.head()?;
    info!(
        "HEAD is {} ({})",
        head.shorthand().or(Some("unprintable")).unwrap(),
        head.name().or(Some("unprintable")).unwrap()
    );

    info!(
        "parsing the entirety of history (abridged){}",
        if args.depth > 0 {
            format!(", max depth {}", args.depth)
        } else {
            "".to_string()
        }
    );

    let revs = serialized_revs_from_repo(&repo, args.depth)?;
    info!("found {} revs in history tree", revs.len());

    let references = referenced_oids_and_paths(&args.repository, &revs)?;
    render_objects(&args.repository, oid_target.to_path_buf(), &references)?;

    info!("recursively rendering {} commit trees", revs.len());

    let tl = ThreadLocal::new();
    let mut revset = DashSet::with_capacity(revs.len());

    for rev in revs {
        revset.insert(rev);
    }

    loop {
        let subtrees = DashSet::new();
        #[allow(clippy::redundant_closure)]
        revset
            .par_iter()
            .map(|rev| {
                let oid = &Oid::from_bytes(rev.key())?;
                let repo = tl.get_or(|| Repository::open(&args.repository).unwrap());
                let tree = repo
                    .find_commit(*oid)
                    .and_then(|commit| commit.tree())
                    .unwrap_or_else(|_| repo.find_tree(*oid).unwrap());
                let objects = tree
                    .iter()
                    .map(|entry| renderable_tree_object_gross_side_effects(repo, entry, &subtrees))
                    .collect();

                let rendering = TreeView {
                    tree_oid: oid,
                    tree_alias: &None,
                    objects: &objects,
                };
                let output_filename = {
                    let mut target = (*tree_target).clone();
                    target.push(format!("{}.html", oid));
                    target
                };
                let mut output = File::create(&output_filename)?;
                output.write_all(rendering.to_string().as_bytes())?;
                Ok(())
            })
            // for now I have no real interest in the results, so just discard them
            .for_each(|x: Result<()>| drop(x));

        if subtrees.is_empty() {
            break;
        }

        revset = subtrees;
    }

    info!("well gawsh darn, looks like we're done here");

    Ok(())
}

// for the love of god, get this subtree side effect bullshit out of here, return a tuple or
// something
//
// TODO FIXME
fn renderable_tree_object_gross_side_effects(
    repo: &Repository,
    entry: TreeEntry,
    subtrees: &DashSet<Vec<u8>>,
) -> Option<RenderableTreeObject> {
    match entry.kind() {
        Some(ObjectType::Tree) => {
            subtrees.insert(entry.id().as_bytes().to_vec());

            Some(RenderableTreeObject::Tree {
                oid: entry.id(),
                name: entry.name().unwrap().to_string(),
            })
        }
        Some(ObjectType::Blob) => {
            let blob = entry.to_object(repo).unwrap().peel_to_blob().unwrap();
            let blob_id = blob.id();

            if blob.is_binary() {
                Some(RenderableTreeObject::BinaryFile {
                    oid: blob_id,
                    name: entry.name().unwrap().to_string(),
                })
            } else {
                Some(RenderableTreeObject::TextFile {
                    oid: blob_id,
                    name: entry.name().unwrap().to_string(),
                })
            }
        }
        _ => None,
    }
}

/// libgit2 isn't threadsafe as a general rule, so git2-rs likewise doesn't implement Send for...
/// anything. so this is our hack: take the OID objects, serialize them to 20-byte u8 vectors
/// (because, interestingly, these are [u8]s and not [u8; 20]s implementing Sized, and I can't
/// figure out how to coerce the type system into believing they're [u8; 20]s) that Rayon can
/// actually do something with, and then farm those out to worker threads (that then have to take
/// the overhead of opening a Repository and deserializing the OID.... very efficient, wow)
fn serialized_revs_from_repo(repo: &Repository, depth: usize) -> Result<SerializedOids> {
    let revwalk = {
        let mut revwalk = repo.revwalk()?;
        revwalk.push_head()?;
        revwalk
    };

    if depth > 0 {
        Ok(revwalk
            .into_iter()
            .take(depth)
            .map(revwalk_mapper)
            .collect()) // no impl for Map<Revwalk...> to rayon::IntoParallelRefIterator
    } else {
        Ok(revwalk.into_iter().map(revwalk_mapper).collect())
    }
}

fn revwalk_mapper(rev: core::result::Result<Oid, git2::Error>) -> SerializedOid {
    (*rev.unwrap().as_bytes()).to_vec()
}

// eventually this tool should be able to render just N>0 arbitrary commit(s) as specified at
// CLI, and not implicitly walk the entire HEAD tree, which means the naive shortcut of just
// rendering all objects in the ODB isn't suitable. instead, we need to keep track of the OIDs
// that are actually referenced in commits we actually need to render, and then queue up jobs
// for each of those objects
#[allow(clippy::ptr_arg)]
fn referenced_oids_and_paths(repo_path: &str, revs: &SerializedOids) -> Result<ReferencedOids> {
    let all_oids = DashSet::new();
    let relevant_oids = DashMap::new();
    let fname_cache = DashMap::new();
    let tl = ThreadLocal::new();

    revs.par_iter().for_each(|rev| {
        let repo = tl.get_or(|| Repository::open(&repo_path).unwrap());
        let commit = repo.find_commit(Oid::from_bytes(rev).unwrap()).unwrap();
        let commit_tree = commit.tree().unwrap();
        commit_tree
            .walk(TreeWalkMode::PreOrder, |_, entry| {
                if entry.kind() == Some(ObjectType::Tree) {
                    return TreeWalkResult::Ok;
                }

                let oid = entry.id();

                // DashSet.insert returns false if key **did** already exist, allowing us to skip
                // some disk I/O if we already know about an object
                if !all_oids.insert(oid) {
                    return TreeWalkResult::Ok;
                }

                // ensure the OID actually resolves to something reasonable, otherwise
                // complain about it and move on
                if repo.find_object(oid, None).is_err() {
                    error!("entity {} is unreachable in ODB, skipping", oid);
                    return TreeWalkResult::Ok;
                }

                let fname = entry.name().unwrap().to_string();
                let cache_key = fname_cache.hash_usize(&fname);
                fname_cache.insert(cache_key, fname);
                relevant_oids.insert(oid, cache_key);

                TreeWalkResult::Ok
            })
            .unwrap();
    });

    Ok(ReferencedOids {
        oids: relevant_oids,
        filenames: fname_cache,
    })
}

fn render_objects(repo_path: &str, output_target: PathBuf, refs: &ReferencedOids) -> Result<()> {
    info!("rendering {} non-binary blob objects", refs.oids.len());

    let class_style = ClassStyle::SpacedPrefixed { prefix: "gawsh-" };
    let theme_set = ThemeSet::load_defaults();
    let default_style = Arc::new(
        css_for_theme_with_class_style(
            theme_set.themes.get("InspiredGitHub").unwrap(),
            class_style,
        )
        .into_bytes(),
    );

    let tl = ThreadLocal::new();
    #[allow(clippy::redundant_closure)]
    refs.oids
        .par_iter()
        .map(|it| {
            let repo = tl.get_or(|| Repository::open(&repo_path).unwrap());
            let oid = it.key();
            let fname_cache_hash = it.value();
            let blob = repo.find_object(*oid, None)?.peel_to_blob()?;
            if blob.is_binary() {
                return Ok(());
            }

            let content = std::str::from_utf8(blob.content())?;
            let fname = refs
                .filenames
                .get(fname_cache_hash)
                .ok_or_else(|| anyhow!("could not find interned filename string"))?;
            let syntax_set = SyntaxSet::load_defaults_newlines();
            let syntax = syntax_set
                .find_syntax_by_first_line(content)
                .or_else(|| {
                    syntax_set.find_syntax_by_extension(
                        Path::new(fname.value())
                            .extension()
                            .map(|ext| ext.to_str().or(Some("")).unwrap())
                            .or(Some(""))?,
                    )
                })
                .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
            let mut html_generator =
                ClassedHTMLGenerator::new_with_class_style(syntax, &syntax_set, class_style);
            for line in LinesWithEndings::from(content) {
                html_generator.parse_html_for_line_which_includes_newline(line);
            }
            let output_html = html_generator.finalize();

            let output_filename = {
                let mut target = output_target.clone();
                target.push(format!("{}.html", oid));
                target
            };
            let mut output = File::create(&output_filename)?;
            output.write_all(b"<style>")?;
            output.write_all(&default_style)?;
            output.write_all(b"</style>")?;

            let rendering = RenderedObject {
                lines: output_html.lines().map(String::from).collect(),
            };

            output.write_all(rendering.to_string().as_bytes())?;

            debug!(
                "rendered {}",
                output_filename
                    .to_str()
                    .ok_or_else(|| anyhow!("could not convert output filename to string"))?
            );

            Ok(())
        })
        // for now I have no real interest in the results, so just discard them
        .for_each(|x: Result<()>| drop(x));

    Ok(())
}

fn duplicate_file_on_disk<S: AsRef<Path>>(
    behavior: &DuplicateLinkageBehavior,
    source: &S,
    target: &S,
) -> Result<(), std::io::Error> {
    match behavior {
        DuplicateLinkageBehavior::Copy => std::fs::copy(source, target).map(|_| ()),
        DuplicateLinkageBehavior::HardLink => std::fs::hard_link(source, target),
        #[cfg(unix)]
        DuplicateLinkageBehavior::SymLink => std::os::unix::fs::symlink(source, target),
    }
}

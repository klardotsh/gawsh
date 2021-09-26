#[macro_use]
extern crate anyhow;
extern crate argh;
extern crate chrono;
extern crate colog;
extern crate dashmap;
extern crate git2;
#[macro_use]
extern crate log;
extern crate markup;
extern crate num_cpus;
extern crate rayon;
extern crate sled;
extern crate syntect;
extern crate thread_local;

mod sled_helpers;

use anyhow::Result;
use argh::{FromArgValue, FromArgs};
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use core::ops::Deref;
use dashmap::{DashMap, DashSet};
use git2::{ObjectType, Oid, Repository, TreeEntry, TreeWalkMode, TreeWalkResult};
use rayon::prelude::*;
use sled_helpers::concatenate_merge;
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
    /// Client-side immediate redirect instruction to a given URL
    // Technically, <meta http-equiv="refresh"> should only work in a <head>, but even lynx and w3m
    // seem to respect this tag existing basically anywhere, so we're going to roll with this hack.
    ImmediateRedirectionInstruction<'a>(target: &'a str) {
        meta["http-equiv"="refresh", content=format!("0; url='{}'", target)] {}
    }

    RenderedObject<'a>(lines: &'a [String]) {
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

    TreeView<'a, OidToString>(
        tree_oid: &'a Oid,
        aliases: Option<&'a [TreeAlias]>,
        tree_modification_time: Option<&'a DateTime<Utc>>,
        objects: &'a [Option<RenderableTreeObject>],
        parent: Option<&'a Oid>,
        tree_link_generator: Option<OidToString>,
    )
        where
        OidToString: Fn(&'a Oid) -> String,
    {
        div."gawsh-tree-header" {
            @if let Some(aliases) = aliases {
                @for alias in aliases.iter() {
                    @match alias {
                        TreeAlias::Head(head) => { span."gawsh-tree-header-alias-head" { @head } }
                        TreeAlias::Tag(tag) => { span."gawsh-tree-header-alias-tag" { @tag } }
                    }
                }

                span."gawsh-tree-header-aliased-commitish" {
                    @format!("({})", pretty_oid(tree_oid))
                }
            } else {
                span."gawsh-tree-header-unaliased-commitish" {
                    @pretty_oid(tree_oid)
                }
            }

            @if let Some(parent) = parent {
                span."gawsh-tree-header-parent-wrapper" {
                    "(parent: "
                    span."gawsh-tree-header-parent-commitish" {
                        @if let Some(gen) = tree_link_generator {
                            a[href=gen(parent)] {
                                @pretty_oid(parent)
                            }
                        } else {
                            @pretty_oid(parent)
                        }
                    }
                    ")"
                }
            }

            @if let Some(modtime) = tree_modification_time {
                span."gawsh-tree-header-modification-time" {
                    @modtime.to_rfc2822()
                }
            }
        }
        table."gawsh-tree-contents" {
            @for obj in objects.iter() {
                @if let Some(obj) = obj {
                    tr."gawsh-tree-line" {
                        td."gawsh-tree-line-name" {
                            @if let Some(link) = obj.relative_link(tree_oid) {
                                // TODO FIXME no root-relative
                                a[href=format!("/{}", link)] {
                                    @obj.name()
                                }
                            } else {
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
#[derive(FromArgs, PartialEq, Debug)]
struct CmdArgs {
    /// be chatty
    #[argh(switch, short = 'v')]
    verbose: bool,

    /// limit history walk depth to N commits. defaults to 0, meaning no limit (recurse from HEAD
    /// to the beginning of discoverable history). this allows for rendering only the tip of a
    /// branch, particularly useful if ancestors have already been rendered (or with
    /// --no-history-links)
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

    /// don't generate links to past commits; only allow linking to objects referenced
    /// within the same commit tree. generally useful only if combined with --depth=1, to render
    /// just the tip of HEAD but no ancestors, while also ensuring no broken links
    #[argh(switch, short = 'H')]
    no_history_links: bool,

    /// don't zstd-compress the embedded workspace database
    #[argh(switch)]
    #[cfg(feature = "workspace-compression")]
    no_workspace_compression: bool,
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

#[derive(Debug)]
struct ReferencedOids {
    oids: InternedFilenamesByOid,
    filenames: InternedFilenames,
}

type InternedFilenamesByOid = DashMap<Oid, usize>;
type InternedFilenames = DashMap<usize, String>;

type SerializedOid = Vec<u8>;
type SerializedOids = Vec<SerializedOid>;

#[derive(Debug, Eq, Hash, PartialEq)]
// this has to be pub to make markup.rs happy
//
// these should probably be interned somewhere much as filenames are
pub enum RenderableTreeObject {
    Tree {
        oid: Oid,
        name: String,
        topography: PathBuf,
    },
    TextFile {
        oid: Oid,
        name: String,
        topography: PathBuf,
    },
    BinaryFile {
        oid: Oid,
        name: String,
        topography: PathBuf,
    },
}

impl RenderableTreeObject {
    pub fn link(&self) -> Option<String> {
        match self {
            RenderableTreeObject::Tree { oid, .. } => Some(generate_tree_link(oid)),
            RenderableTreeObject::TextFile { oid, .. } => Some(generate_oid_link(oid)),
            RenderableTreeObject::BinaryFile { .. } => None,
        }
    }

    pub fn relative_link(&self, tree: &Oid) -> Option<String> {
        match self {
            RenderableTreeObject::TextFile {
                oid: _,
                name,
                topography,
            }
            | RenderableTreeObject::Tree {
                oid: _,
                name,
                topography,
            } => {
                let mut pb = PathBuf::with_capacity(topography.iter().count() + 2);
                pb.push("tree");
                pb.push(tree.to_string());
                for ele in topography.iter() {
                    pb.push(ele);
                }
                pb.push(name);

                Some(pb.as_path().to_string_lossy().into_owned())
            }
            RenderableTreeObject::BinaryFile { .. } => None,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            RenderableTreeObject::Tree { name, .. }
            | RenderableTreeObject::TextFile { name, .. }
            | RenderableTreeObject::BinaryFile { name, .. } => name,
        }
    }
}

#[test]
fn test_rto_relative_link() -> Result<()> {
    assert_eq!(
        "tree/7b1d3f17c47cce7788f74a2a620c5eb4034f6ff3/README.md",
        RenderableTreeObject::TextFile {
            oid: Oid::from_str("f504bdfd6fee4f3fd29c0611d95b1ae24bd6e6cd")?,
            topography: PathBuf::new(),
            name: "README.md".to_string(),
        }
        .relative_link(&Oid::from_str("7b1d3f17c47cce7788f74a2a620c5eb4034f6ff3")?)
        .ok_or_else(|| anyhow!("relative link is None"))?
    );

    assert_eq!(
        "tree/7b1d3f17c47cce7788f74a2a620c5eb4034f6ff3/somedir/USAGE.txt",
        RenderableTreeObject::TextFile {
            oid: Oid::from_str("f504bdfd6fee4f3fd29c0611d95b1ae24bd6e6cd")?,
            topography: PathBuf::from("somedir"),
            name: "USAGE.txt".to_string(),
        }
        .relative_link(&Oid::from_str("7b1d3f17c47cce7788f74a2a620c5eb4034f6ff3")?)
        .ok_or_else(|| anyhow!("relative link is None"))?
    );

    assert_eq!(
        "tree/7b1d3f17c47cce7788f74a2a620c5eb4034f6ff3/somedir/otherthing/USAGE.txt",
        RenderableTreeObject::TextFile {
            oid: Oid::from_str("f504bdfd6fee4f3fd29c0611d95b1ae24bd6e6cd")?,
            topography: PathBuf::from("somedir/otherthing"),
            name: "USAGE.txt".to_string(),
        }
        .relative_link(&Oid::from_str("7b1d3f17c47cce7788f74a2a620c5eb4034f6ff3")?)
        .ok_or_else(|| anyhow!("relative link is None"))?
    );

    assert_eq!(
        "tree/7b1d3f17c47cce7788f74a2a620c5eb4034f6ff3/topfolder",
        RenderableTreeObject::TextFile {
            oid: Oid::from_str("f504bdfd6fee4f3fd29c0611d95b1ae24bd6e6cd")?,
            topography: PathBuf::new(),
            name: "topfolder".to_string(),
        }
        .relative_link(&Oid::from_str("7b1d3f17c47cce7788f74a2a620c5eb4034f6ff3")?)
        .ok_or_else(|| anyhow!("relative link is None"))?
    );

    assert_eq!(
        "tree/7b1d3f17c47cce7788f74a2a620c5eb4034f6ff3/topfolder/nestedfolder",
        RenderableTreeObject::TextFile {
            oid: Oid::from_str("f504bdfd6fee4f3fd29c0611d95b1ae24bd6e6cd")?,
            topography: PathBuf::from("topfolder"),
            name: "nestedfolder".to_string(),
        }
        .relative_link(&Oid::from_str("7b1d3f17c47cce7788f74a2a620c5eb4034f6ff3")?)
        .ok_or_else(|| anyhow!("relative link is None"))?
    );

    Ok(())
}

/// Aliases a tree can hold; almost always only used for commit trees.
#[derive(Debug)]
pub enum TreeAlias {
    /// A branch head, eg. "main"
    Head(String),

    /// A tag, eg. "v1.0.0"
    Tag(String),
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

    #[cfg(not(feature = "workspace-compression"))]
    let db = sled::Config::default()
        .path(".gawsh.sled".to_owned())
        .mode(sled::Mode::HighThroughput)
        .flush_every_ms(Some(2000))
        .open()?;

    #[cfg(feature = "workspace-compression")]
    let db = sled::Config::default()
        .path(".gawsh.sled".to_owned())
        .mode(sled::Mode::HighThroughput)
        .flush_every_ms(Some(2000))
        .use_compression(!args.no_workspace_compression)
        .open()?;

    db.set_merge_operator(concatenate_merge);

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
        target.push("refs");
        Arc::new(target)
    };
    drop(output_root);
    create_dir_all(&*oid_target)?;
    create_dir_all(&*tree_target)?;
    create_dir_all(&*ref_target)?;

    let repo = Repository::open(&args.repository)?;
    let rev_state = Arc::new(db.open_tree("revs")?);
    serialized_revs_from_repo(&repo, &rev_state, args.depth)?;
    info!("found {} revs in history tree", rev_state.len());

    let oids = Arc::new(db.open_tree("oids")?);
    let oids_todo = Arc::new(db.open_tree("oids_todo")?);
    oids_todo.clear()?;
    oids_todo.set_merge_operator(concatenate_merge);
    let oids_dlq = Arc::new(db.open_tree("oids_dlq")?);
    oids_dlq.clear()?;
    determine_oids_to_render(&args.repository, &rev_state, &oids, &oids_todo, &oids_dlq)?;

    render_text_blobs(&args.repository, &oids_todo, &oids)?;

    //info!("recursively rendering {} commit trees", revs.len());

    let history_links = !args.no_history_links;

    /*
    loop {
        let subtrees = DashSet::new();
        #[allow(clippy::redundant_closure)]
        revset
            .par_iter()
            .map(|rev| {
                let (raw_oid, topology) = rev.key();
                debug!("figuring out trees for {:?}", topology);
                let oid = &Oid::from_bytes(raw_oid)?;
                debug!("rendering tree for {}", oid);
                let repo = tl.get_or(|| Repository::open(&args.repository).unwrap());
                let commit = repo.find_commit(*oid)?;
                render_commit_to_disk(&commit)
            })
            // for now I have no real interest in the results, so just discard them
            .for_each(|x: Result<()>| drop(x));

        if subtrees.is_empty() {
            break;
        }

        revset = subtrees;
    }
    */

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
    subtrees: &DashSet<(Vec<u8>, PathBuf)>,
    topology: &Path,
) -> Option<RenderableTreeObject> {
    match entry.kind() {
        Some(ObjectType::Tree) => {
            let mut new_pb = topology.to_path_buf();
            new_pb.push(entry.name().unwrap());
            subtrees.insert((entry.id().as_bytes().to_vec(), new_pb));

            Some(RenderableTreeObject::Tree {
                oid: entry.id(),
                name: entry.name().unwrap().to_string(),
                topography: topology.to_path_buf(),
            })
        }
        Some(ObjectType::Blob) => {
            let blob = entry.to_object(repo).unwrap().peel_to_blob().unwrap();
            let blob_id = blob.id();

            if blob.is_binary() {
                Some(RenderableTreeObject::BinaryFile {
                    oid: blob_id,
                    name: entry.name().unwrap().to_string(),
                    topography: topology.to_path_buf(),
                })
            } else {
                Some(RenderableTreeObject::TextFile {
                    oid: blob_id,
                    name: entry.name().unwrap().to_string(),
                    topography: topology.to_path_buf(),
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
fn serialized_revs_from_repo(repo: &Repository, db: &sled::Tree, depth: usize) -> Result<()> {
    let revwalk = {
        let mut revwalk = repo.revwalk()?;
        revwalk.push_glob("*")?;
        revwalk
    };

    for rev in revwalk {
        let rev = rev?;
        db.insert(rev, &[0])?;
    }

    Ok(())
}

fn revwalk_mapper(rev: core::result::Result<Oid, git2::Error>) -> SerializedOid {
    (*rev.unwrap().as_bytes()).to_vec()
}

// eventually this tool should be able to render just N>0 arbitrary commit(s) as specified at
// CLI, and not implicitly walk the entire HEAD tree, which means the naive shortcut of just
// rendering all objects in the ODB isn't suitable. instead, we need to keep track of the OIDs
// that are actually referenced in commits we actually need to render, and then queue up jobs
// for each of those objects
fn determine_oids_to_render(
    repo_path: &str,
    rev_db: &dyn Deref<Target = sled::Tree>,
    oid_rendered_db: &(dyn Deref<Target = sled::Tree> + Sync),
    oid_todo_db: &(dyn Deref<Target = sled::Tree> + Sync),
    oid_dlq_db: &(dyn Deref<Target = sled::Tree> + Sync),
) -> Result<()> {
    let tl = Arc::new(ThreadLocal::new());

    rev_db
        .iter()
        .par_bridge()
        .try_for_each(|rev| {
            let (rev, _) = rev.unwrap();
            let repo = tl.get_or(|| Repository::open(&repo_path).unwrap());
            repo.find_commit(Oid::from_bytes(&rev)?)?.tree()?.walk(
                TreeWalkMode::PreOrder,
                |_, entry| {
                    // TODO pre-render tree stubs for later even though they're pretty cheap?
                    if entry.kind() == Some(ObjectType::Tree) {
                        return TreeWalkResult::Ok;
                    }

                    let oid = entry.id();
                    let oid_bytes = oid.as_bytes();

                    // we don't care about files we've already written, files we already know we need
                    // to render, and files we already know are inaccessible, so bail early on these
                    if oid_rendered_db.contains_key(&oid_bytes).unwrap()
                        || oid_todo_db.contains_key(&oid_bytes).unwrap()
                        || oid_dlq_db.contains_key(&oid_bytes).unwrap()
                    {
                        return TreeWalkResult::Ok;
                    }

                    // ensure the OID actually resolves to something reasonable, otherwise
                    // complain about it and move on
                    if repo.find_object(oid, None).is_err() {
                        error!("entity {} is unreachable in ODB", oid);
                        oid_dlq_db.insert(oid_bytes, &[0]).unwrap();
                        return TreeWalkResult::Ok;
                    }

                    oid_todo_db
                        .merge(oid_bytes, entry.name_bytes())
                        .map_or_else(
                            |err| {
                                error!("failed to walk OID {}: {:?}", oid, err);
                                TreeWalkResult::Abort
                            },
                            |_| {
                                debug!("should render object {}", oid);
                                TreeWalkResult::Ok
                            },
                        )
                },
            )
        })
        .map_err(|err| anyhow!("libgit2 reported error: {}"))
}

fn render_text_blobs(
    repo_path: &str,
    todo_db: &dyn Deref<Target = sled::Tree>,
    target_db: &(dyn Deref<Target = sled::Tree> + Sync),
) -> Result<()> {
    info!("rendering {} text blobs", todo_db.iter().count(),);

    let class_style = ClassStyle::SpacedPrefixed { prefix: "gawsh-" };
    let theme_set = ThemeSet::load_defaults();
    let default_style = Arc::new(
        css_for_theme_with_class_style(
            theme_set.themes.get("InspiredGitHub").unwrap(),
            class_style,
        )
        .into_bytes(),
    );

    let tl = Arc::new(ThreadLocal::new());
    todo_db.iter().par_bridge().try_for_each(|it| {
        let (oid, filenames) = it?;
        let oid = Oid::from_bytes(&oid)?;
        let filenames: Vec<&str> = filenames
            .split(|c| c == &0)
            .map(|fname| std::str::from_utf8(fname).unwrap())
            .collect();

        if filenames.len() > 1 {
            let extensions: DashSet<&str> = filenames
                .iter()
                .map(|name| {
                    Path::new(name)
                        .extension()
                        .map(|ext| ext.to_str().or(Some("")).unwrap())
                        .or(Some(""))
                        .unwrap()
                })
                .collect();

            if extensions.len() > 1 {
                warn!("file {} had multiple extensions, only the first will be used for syntax highlighting: {:?}", oid, extensions);
            }
        }

        render_text_blob(
            &class_style,
            &tl,
            repo_path,
            &oid,
            filenames.first().or(Some(&"")).ok_or_else(|| anyhow!("internal error determining filename or empty string for blob"))?,
            target_db,
        )
    })
}

fn render_text_blob(
    hl_class_style: &ClassStyle,
    tl: &dyn Deref<Target = ThreadLocal<Repository>>,
    repo_path: &str,
    oid: &Oid,
    filename: &str,
    db: &sled::Tree,
) -> Result<()> {
    let repo = tl.get_or(|| Repository::open(repo_path).unwrap());
    let blob = repo.find_object(*oid, None)?.peel_to_blob()?;
    if blob.is_binary() {
        return Ok(());
    }

    // already in testing I ran into repos with non-UTF8-encodable content, so on those rare
    // occasions we'll eat the conversion costs to insert the replacement characters
    let content = String::from_utf8_lossy(blob.content());

    let syntax_set = SyntaxSet::load_defaults_newlines();
    let syntax = syntax_set
        .find_syntax_by_first_line(&content)
        .or_else(|| {
            syntax_set.find_syntax_by_extension(
                Path::new(filename)
                    .extension()
                    .map(|ext| ext.to_str().or(Some("")).unwrap())
                    .or(Some(""))?,
            )
        })
        .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
    let rendered_object = {
        let mut html_generator =
            ClassedHTMLGenerator::new_with_class_style(syntax, &syntax_set, *hl_class_style);
        for line in LinesWithEndings::from(&content) {
            html_generator.parse_html_for_line_which_includes_newline(line);
        }
        let output_html = html_generator.finalize();
        RenderedObject {
            lines: &output_html
                .lines()
                .map(String::from)
                .collect::<Vec<String>>(),
        }
    };

    db.insert(oid.as_bytes(), rendered_object.to_string().as_bytes())?;

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

// TODO FIXME no root-relative links
fn generate_tree_link(oid: &Oid) -> String {
    format!("/tree/{}", oid)
}

// TODO FIXME no root-relative links
fn generate_oid_link(oid: &Oid) -> String {
    format!("/oid/{}.html", oid)
}

fn render_commit_to_disk(commit: &git2::Commit) -> Result<()> {
    //render_tree_to_disk(commit.tree())
    Ok(())
}

fn render_tree_to_disk(tree: &git2::Tree) -> Result<()> {
    /*
    let objects: Vec<Option<RenderableTreeObject>> = tree
        .iter()
        .map(|entry| {
            renderable_tree_object_gross_side_effects(repo, entry, &subtrees, &topology)
        })
        .collect();
    let parent = repo
        .find_commit(*oid)
        .map(|commit| commit.parent_id(0).ok())
        .unwrap_or(None);
    let modification_time = repo
        .find_commit(*oid)
        .map(|commit| {
            let time = commit.time();
            let offset = FixedOffset::east(time.offset_minutes() * 60);
            Some(offset.timestamp(time.seconds(), 0).with_timezone(&Utc))
        })
        .unwrap_or(None);
    let rendering = TreeView {
        tree_oid: oid,
        aliases: None,
        tree_modification_time: modification_time.as_ref(),
        objects: &objects,
        parent: parent.as_ref(),
        tree_link_generator: if history_links {
            Some(generate_tree_link)
        } else {
            None
        },
    };
    */

    // ensure all referenced objects have warp-to files
    /*
    for obj in objects {

    }
    */

    /*
    let output_filename = {
        let mut target = (*tree_target).clone();
        target.push(oid.to_string());
        target.push("index.html");
        target
    };
    create_dir_all(&*output_filename.parent().unwrap())?;
    let mut output = File::create(&output_filename)?;
    output.write_all(rendering.to_string().as_bytes())?;
    */
    Ok(())
}

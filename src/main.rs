#[macro_use]
extern crate anyhow;
extern crate argh;
extern crate colog;
extern crate dashmap;
extern crate git2;
#[macro_use]
extern crate log;
extern crate rayon;
extern crate syntect;

use anyhow::Result;
use argh::{FromArgValue, FromArgs};
use dashmap::{DashMap, DashSet};
use git2::{ObjectType, Oid, Repository, TreeWalkMode, TreeWalkResult};
use rayon::prelude::*;
use std::fs::{create_dir_all, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use syntect::highlighting::ThemeSet;
use syntect::html::{css_for_theme_with_class_style, ClassStyle, ClassedHTMLGenerator};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

/// gawsh generates a static HTML portrait of a Git repository
#[derive(FromArgs, PartialEq, Debug)]
struct CmdArgs {
    #[argh(switch, description = "be chatty")]
    verbose: bool,

    #[argh(
        option,
        description = "repository to operate on",
        default = "String::from(\".\")"
    )]
    repository: String,

    #[argh(
        option,
        description = "output directory for rendered files, will be created if it doesn't exist",
        default = "String::from(\".gawsh-output\")"
    )]
    output: String,

    #[argh(
        option,
        description = "templating behavior for embedding rendered Objects into tree files",
        default = "TemplatingBehavior::Disabled"
    )]
    templating_behavior: TemplatingBehavior,

    #[argh(switch, description = "prefix highlighting HTML classes with gawsh-")]
    use_class_prefix: bool,
}

/// To save disk space, gawsh can render Objects (the files stored in the Git repository) to
/// snippet files which can then be referenced in an "include" statement from all pages that need
/// to show that version of the file in question. These statements inherently vary by templating
/// engine.
///
/// This functionality is disabled by default; all objects will simply be inline concatenated into
/// referring files no matter the disk space cost, as this is the only guaranteed-safe-everywhere
/// behavior that doesn't take runtime dependencies.
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

type InternedFilenamesByOid = DashMap<Oid, usize>;
type InternedFilenames = DashMap<usize, String>;

#[derive(Debug)]
struct ReferencedOids {
    oids: InternedFilenamesByOid,
    filenames: InternedFilenames,
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

    let repo = Repository::open(&args.repository)?;
    let head = repo.head()?;
    info!(
        "HEAD is {} ({})",
        head.shorthand().or(Some("unprintable")).unwrap(),
        head.name().or(Some("unprintable")).unwrap()
    );

    let references = referenced_oids_and_paths(&repo, &args.repository)?;
    render_objects(
        args.use_class_prefix,
        &args.repository,
        &args.output,
        &references,
    )?;

    Ok(())
}

// eventually this tool should be able to render just N>0 arbitrary commit(s) as specified at
// CLI, and not implicitly walk the entire HEAD tree, which means the naive shortcut of just
// rendering all objects in the ODB isn't suitable. instead, we need to keep track of the OIDs
// that are actually referenced in commits we actually need to render, and then queue up jobs
// for each of those objects
fn referenced_oids_and_paths(repo: &Repository, repo_path: &str) -> Result<ReferencedOids> {
    let broken_oids = DashSet::new();
    let relevant_oids = DashMap::new();
    let fname_cache = DashMap::new();
    let revwalk = {
        let mut revwalk = repo.revwalk()?;
        revwalk.push_head()?;
        revwalk
    };

    // libgit2 isn't threadsafe as a general rule, so git2-rs likewise doesn't implement Send
    // for... anything. so this is our hack: take the OID objects, serialize them to 20-byte u8
    // vectors (because, interestingly, these are [u8]s and not [u8; 20]s implementing Sized, and I
    // can't figure out how to coerce the type system into believing they're [u8; 20]s) that Rayon
    // can actually do something with, and then farm those out to worker threads (that then have to
    // take the overhead of opening a Repository and deserializing the OID.... very efficient, wow)
    revwalk
        .into_iter()
        .map(|rev| (*rev.unwrap().as_bytes()).to_vec())
        .collect::<Vec<Vec<u8>>>() // no impl for Map<Revwalk...> to rayon::IntoParallelRefIterator
        .par_iter()
        .for_each(|rev| {
            let repo = match Repository::open(repo_path) {
                Ok(repo) => repo,
                Err(e) => panic!("failed to open: {}", e),
            };
            let commit = repo.find_commit(Oid::from_bytes(rev).unwrap()).unwrap();
            let commit_tree = commit.tree().unwrap();
            commit_tree
                .walk(TreeWalkMode::PreOrder, |_, entry| {
                    if entry.kind() == Some(ObjectType::Tree) {
                        return TreeWalkResult::Ok;
                    }

                    let oid = entry.id();

                    if repo.find_object(oid, None).is_err() {
                        // DashSet.insert returns true if key **did not** exist
                        if broken_oids.insert(oid) {
                            error!("entity {} is unreachable in ODB, skipping", oid);
                        }

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

fn render_objects(
    use_class_prefix: bool,
    repo_path: &str,
    output_path: &str,
    refs: &ReferencedOids,
) -> Result<()> {
    info!("rendering {} non-binary blob objects", refs.oids.len());

    let output_root = PathBuf::from(output_path);
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
    drop(output_root); // this conveniently also shuts clippy up

    create_dir_all(&*oid_target)?;
    create_dir_all(&*tree_target)?;

    let class_style = if use_class_prefix {
        ClassStyle::SpacedPrefixed { prefix: "gawsh-" }
    } else {
        ClassStyle::Spaced
    };
    let theme_set = ThemeSet::load_defaults();
    let default_style = Arc::new(
        css_for_theme_with_class_style(
            theme_set.themes.get("InspiredGitHub").unwrap(),
            class_style,
        )
        .into_bytes(),
    );

    let _results: Vec<Result<()>> = refs
        .oids
        .par_iter()
        .map(|it| {
            let repo = Repository::open(&repo_path)?;
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
                let mut target = (*oid_target).clone();
                target.push(format!("{}.html", oid));
                target
            };
            let mut output = File::create(&output_filename)?;
            output.write_all(b"<style>")?;
            output.write_all(&default_style)?;
            output.write_all(b"</style>")?;
            output.write_all(b"<pre>")?;
            output.write_all(&output_html.into_bytes())?;
            output.write_all(b"</pre>")?;

            debug!(
                "rendered {}",
                output_filename
                    .to_str()
                    .ok_or_else(|| anyhow!("could not convert output filename to string"))?
            );

            Ok(())
        })
        .collect();

    Ok(())
}

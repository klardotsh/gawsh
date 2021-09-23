extern crate anyhow;
extern crate argh;
extern crate colog;
extern crate dashmap;
extern crate git2;
extern crate indexmap;
#[macro_use]
extern crate log;
extern crate num_cpus;
extern crate syntect;
extern crate threadpool;

use anyhow::Error;
use argh::{FromArgValue, FromArgs};
use dashmap::{DashMap, DashSet};
use git2::{ObjectType, Oid, Repository, TreeWalkMode, TreeWalkResult};
use indexmap::IndexSet;
use std::fs::{create_dir_all, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use syntect::highlighting::ThemeSet;
use syntect::html::{css_for_theme_with_class_style, ClassStyle, ClassedHTMLGenerator};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use threadpool::ThreadPool;

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

    #[argh(
        option,
        short = 'j',
        description = "number of parallel rendering jobs to run, default is number of CPUs",
        default = "num_cpus::get()"
    )]
    jobs: usize,

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
    fn from_arg_value(val: &str) -> Result<Self, String> {
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

#[derive(Debug)]
struct ReferencedOids {
    oids: Arc<DashMap<Oid, usize>>,
    filenames: Arc<RwLock<IndexSet<String>>>,
}

fn main() {
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

    let repo_path = Arc::new(args.repository);

    let repo = match Repository::open(&*repo_path) {
        Ok(repo) => repo,
        Err(e) => panic!("failed to open: {}", e),
    };

    let head = match repo.head() {
        Ok(head) => head,
        Err(e) => panic!("failed to figure out HEAD: {}", e),
    };

    info!(
        "HEAD is {} ({})",
        head.shorthand().or(Some("unprintable")).unwrap(),
        head.name().or(Some("unprintable")).unwrap()
    );

    let pool = ThreadPool::new(args.jobs);
    let referenced = referenced_oids_and_paths(&pool, &repo, &repo_path).unwrap();
    let relevant_oids = referenced.oids;
    let fname_cache = referenced.filenames;

    info!("rendering {} non-binary blob objects", relevant_oids.len());

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
    drop(output_root); // this conveniently also shuts clippy up

    create_dir_all(&*oid_target).unwrap();
    create_dir_all(&*tree_target).unwrap();

    let class_style = if args.use_class_prefix {
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

    for it in relevant_oids.iter() {
        let oid_target = oid_target.clone();
        let oid = Arc::new(*it.key());
        let latest_fname_idx = Arc::new(*it.value());
        let repo_path = repo_path.clone();
        let oid_bytes = Arc::new(oid.as_bytes().to_vec());
        let fname_cache = fname_cache.clone();
        let default_style = default_style.clone();

        pool.execute(move || {
            let repo = match Repository::open(&*repo_path) {
                Ok(repo) => repo,
                Err(e) => panic!("failed to open: {}", e),
            };
            let oid = Oid::from_bytes(&*oid_bytes).unwrap();
            let blob = repo.find_object(oid, None).unwrap().peel_to_blob().unwrap();
            let content = std::str::from_utf8(blob.content()).unwrap();
            let is_binary = blob.is_binary();

            if is_binary {
                return;
            }

            let fname_cache = fname_cache.read().unwrap();
            let fname = fname_cache.get_index(*latest_fname_idx).unwrap();
            let syntax_set = SyntaxSet::load_defaults_newlines();
            let syntax = syntax_set
                .find_syntax_by_first_line(content)
                .or_else(|| {
                    syntax_set.find_syntax_by_extension(
                        Path::new(fname)
                            .extension()
                            .map(|ext| ext.to_str().unwrap())
                            .or(Some(""))
                            .unwrap(),
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
            let mut output = File::create(&output_filename).unwrap();
            output.write_all(b"<style>").unwrap();
            output.write_all(&default_style).unwrap();
            output.write_all(b"</style>").unwrap();
            output.write_all(b"<pre>").unwrap();
            output.write_all(&output_html.into_bytes()).unwrap();
            output.write_all(b"</pre>").unwrap();

            debug!("rendered {}", output_filename.to_str().unwrap());
        });
    }

    pool.join();
}

// eventually this tool should be able to render just N>0 arbitrary commit(s) as specified at
// CLI, and not implicitly walk the entire HEAD tree, which means the naive shortcut of just
// rendering all objects in the ODB isn't suitable. instead, we need to keep track of the OIDs
// that are actually referenced in commits we actually need to render, and then queue up jobs
// for each of those objects
fn referenced_oids_and_paths(
    pool: &ThreadPool,
    repo: &Repository,
    repo_path: &Arc<String>,
) -> Result<ReferencedOids, Error> {
    let broken_oids = Arc::new(DashSet::new());
    let relevant_oids = Arc::new(DashMap::new());
    let fname_cache = Arc::new(RwLock::new(IndexSet::new()));
    let mut revwalk = repo.revwalk()?;
    revwalk.push_head()?;
    let revwalk = revwalk;

    for rev in revwalk {
        let broken_oids = broken_oids.clone();
        let relevant_oids = relevant_oids.clone();
        let fname_cache = fname_cache.clone();
        let repo_path = repo_path.clone();

        pool.execute(move || {
            let repo = match Repository::open(&*repo_path) {
                Ok(repo) => repo,
                Err(e) => panic!("failed to open: {}", e),
            };

            let rev = rev.unwrap();
            let commit = repo.find_commit(rev).unwrap();
            let commit_tree = commit.tree().unwrap();
            commit_tree
                .walk(TreeWalkMode::PreOrder, |_, entry| {
                    if entry.kind() == Some(ObjectType::Tree) {
                        return TreeWalkResult::Ok;
                    }

                    let oid = entry.id();

                    if repo.find_object(oid, None).is_err() {
                        if broken_oids.insert(oid) {
                            error!("entity {} is unreachable in ODB, skipping", oid);
                        }

                        return TreeWalkResult::Ok;
                    }

                    let fname = entry.name().unwrap();

                    let (cache_idx, _) =
                        fname_cache.write().unwrap().insert_full(fname.to_string());

                    relevant_oids.insert(oid, cache_idx);

                    TreeWalkResult::Ok
                })
                .unwrap();
        });
    }

    pool.join();

    Ok(ReferencedOids {
        oids: relevant_oids,
        filenames: fname_cache,
    })
}

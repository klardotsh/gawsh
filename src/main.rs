extern crate anyhow;
extern crate argh;
extern crate console;
extern crate git2;
extern crate indexmap;
extern crate indicatif;
extern crate lazy_static;
extern crate maud;
extern crate num_cpus;
extern crate syntect;
extern crate threadpool;

use argh::{FromArgValue, FromArgs};
use indexmap::IndexSet;
use std::collections::HashMap;
use std::convert::TryInto;
use std::fs::create_dir_all;
use std::io::Write;
use syntect::highlighting::ThemeSet;
use syntect::html::{ClassStyle, ClassedHTMLGenerator};
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

fn main() {
    let args: CmdArgs = argh::from_env();
    let repo_path = std::sync::Arc::new(args.repository);

    let repo = match git2::Repository::open(&*repo_path) {
        Ok(repo) => repo,
        Err(e) => panic!("failed to open: {}", e),
    };

    let head = match repo.head() {
        Ok(head) => head,
        Err(e) => panic!("failed to figure out HEAD: {}", e),
    };

    eprintln!(
        "HEAD is {} ({})",
        head.shorthand().or(Some("unprintable")).unwrap(),
        head.name().or(Some("unprintable")).unwrap()
    );

    // eventually this tool should be able to render just N>0 arbitrary commit(s) as specified at
    // CLI, and not implicitly walk the entire HEAD tree, which means the naive shortcut of just
    // rendering all objects in the ODB isn't suitable. instead, we need to keep track of the OIDs
    // that are actually referenced in commits we actually need to render, and then queue up jobs
    // for each of those objects
    let (relevant_oids, fname_cache) = {
        let mut relevant_oids = HashMap::new();
        let mut fname_cache = IndexSet::new();
        let mut revwalk = repo.revwalk().unwrap();
        revwalk.push_head().unwrap();
        let revwalk = revwalk;

        for rev in revwalk {
            let rev = rev.unwrap();
            let commit = repo.find_commit(rev).unwrap();
            /*
            eprintln!(
                "{} ({} by {}): {}",
                commit.id(),
                commit.author().when().seconds(),
                commit.author().name().unwrap(),
                commit.message().unwrap().trim()
            );
            */

            let commit_tree = commit.tree().unwrap();
            commit_tree
                .walk(git2::TreeWalkMode::PreOrder, |_, entry| {
                    if entry.kind() == Some(git2::ObjectType::Tree) {
                        return git2::TreeWalkResult::Ok;
                    }

                    let oid = entry.id();

                    if repo.find_object(oid, None).is_err() {
                        eprintln!("entity {} is UNREACHABLE in ODB! skipping!", oid);
                        return git2::TreeWalkResult::Ok;
                    }

                    let fname = entry.name().unwrap();

                    let (cache_idx, _) = fname_cache.insert_full(fname.to_string());

                    // see docs for OidMap
                    relevant_oids.insert(oid, cache_idx);

                    git2::TreeWalkResult::Ok
                })
                .unwrap();
        }

        (relevant_oids, std::sync::Arc::new(fname_cache))
    };

    eprintln!("would render {} objects", relevant_oids.len());

    create_dir_all("gawsh_output/oid").unwrap();

    let pool = threadpool::ThreadPool::new(args.jobs);
    let bar = indicatif::ProgressBar::new(relevant_oids.len().try_into().unwrap());
    let class_style = ClassStyle::SpacedPrefixed { prefix: "gawsh-" };
    let theme_set = ThemeSet::load_defaults();

    bar.set_message("rendering objects");

    let default_style = std::sync::Arc::new(
        syntect::html::css_for_theme_with_class_style(
            theme_set.themes.get("InspiredGitHub").unwrap(),
            class_style,
        )
        .into_bytes(),
    );

    for (oid, latest_fname_idx) in relevant_oids {
        let bar = bar.clone();
        let repo_path = repo_path.clone();
        let oid_bytes = std::sync::Arc::new(oid.as_bytes().to_vec());
        let fname_cache = fname_cache.clone();
        let default_style = default_style.clone();

        pool.execute(move || {
            let repo = match git2::Repository::open(&*repo_path) {
                Ok(repo) => repo,
                Err(e) => panic!("failed to open: {}", e),
            };
            let oid = git2::Oid::from_bytes(&*oid_bytes).unwrap();
            let blob = repo.find_object(oid, None).unwrap().peel_to_blob().unwrap();
            let content = std::str::from_utf8(blob.content()).unwrap();
            let is_binary = blob.is_binary();

            if !is_binary {
                let fname = fname_cache.get_index(latest_fname_idx).unwrap();
                let syntax_set = SyntaxSet::load_defaults_newlines();
                let syntax = syntax_set
                    .find_syntax_by_first_line(content)
                    .or_else(|| {
                        syntax_set.find_syntax_by_extension(
                            std::path::Path::new(fname)
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

                let mut output =
                    std::fs::File::create(format!("gawsh_output/oid/{}.html", oid)).unwrap();
                output.write_all(b"<style>").unwrap();
                output.write_all(&default_style).unwrap();
                output.write_all(b"</style>").unwrap();
                output.write_all(b"<pre>").unwrap();
                output.write_all(&output_html.into_bytes()).unwrap();
                output.write_all(b"</pre>").unwrap();

                bar.inc(1);
            }
        });
    }

    pool.join();
    bar.finish_with_message("object stubs rendered");
}

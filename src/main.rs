use eyre::{Result, WrapErr};
use serde::Deserialize;
use skim::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use structopt::StructOpt;
use tmux_interface::TmuxCommand;

#[derive(StructOpt, Debug)]
struct Opts {
    #[structopt(short, long, parse(from_os_str))]
    config_file: Option<PathBuf>,
}

#[derive(Deserialize, Debug)]
struct Config {
    root_dirs: Vec<PathBuf>,
}

fn compute_short_name(p: impl AsRef<Path>) -> String {
    // XXX: So many clones
    let p = p.as_ref();
    let parts: PathBuf = p.components().rev().take(2).collect();
    let result: PathBuf = parts.components().rev().collect();
    result.to_str().unwrap().to_owned()
}

#[derive(Debug)]
struct Selectable {
    path: PathBuf,
    short_name: String,
}

impl SkimItem for Selectable {
    fn text(&self) -> std::borrow::Cow<str> {
        std::borrow::Cow::Borrowed(self.short_name.as_str())
    }
}

fn walk_directory(
    walker: ignore::Walk,
    results: crossbeam_channel::Sender<Arc<dyn SkimItem + 'static>>,
) {
    for every in walker {
        let every = every.unwrap();
        let path = every.path().to_owned();
        let short_name = compute_short_name(&path);
        let e: Arc<dyn SkimItem + 'static> = Arc::new(Selectable { short_name, path });
        results.send(e).unwrap();
    }

    drop(results);
}

struct Session<'a> {
    selectable: &'a Selectable,
    client: TmuxCommand<'a>,
}

impl<'a> Session<'a> {
    fn new(selectable: &'a Selectable) -> Self {
        Self {
            selectable,
            client: TmuxCommand::new(),
        }
    }

    fn exists(&self) -> Result<bool> {
        let output = self
            .client
            .has_session()
            .target_session(&self.selectable.short_name)
            .output()?;
        let status_code = output.code().unwrap_or(1);
        Ok(status_code == 0)
    }

    fn switch_client(&self) -> Result<()> {
        self.client
            .switch_client()
            .target_session(&self.selectable.short_name)
            .output()?;

        Ok(())
    }

    fn create_session(&self) -> Result<()> {
        self.client
            .new_session()
            .detached()
            .start_directory(self.selectable.path.to_str().unwrap())
            .session_name(&self.selectable.short_name)
            .output()?;
        self.switch_client()?;
        Ok(())
    }

    fn tmux_running(&self) -> bool {
        std::env::var("TMUX").map(|_| true).unwrap_or(false)
    }
}

fn replace_home_path(p: &PathBuf) -> PathBuf {
    if p.starts_with("~") {
        dirs::home_dir()
            .unwrap()
            .join(p.strip_prefix("~/").unwrap())
    } else {
        p.clone()
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    color_eyre::install().unwrap();

    let args = Opts::from_args();

    tracing::info!(?args, "starting");

    let config_file_location = args.config_file.unwrap_or(
        dirs::preference_dir()
            .unwrap()
            .join("project")
            .join("config.toml"),
    );
    let config_text =
        std::fs::read_to_string(config_file_location).wrap_err("reading config file")?;
    let config: Config = toml::from_str(&config_text).wrap_err("parsing config file")?;

    let dirs: Vec<_> = config.root_dirs.iter().map(replace_home_path).collect();

    tracing::debug!(?dirs, "using directories");

    let mut builder = ignore::WalkBuilder::new(&dirs[0]);
    builder.max_depth(Some(1));
    builder.filter_entry(|e| e.path().is_dir());
    for dir in dirs.iter().skip(1) {
        builder.add(dir);
    }
    let walker = builder.build();

    let (tx, rx) = crossbeam_channel::bounded(100);
    let options = SkimOptionsBuilder::default()
        .height(Some("100%"))
        .multi(false)
        .exit0(true)
        .final_build()
        .unwrap();

    let handle = std::thread::spawn(move || walk_directory(walker, tx));
    let results = Skim::run_with(&options, Some(rx)).unwrap();
    handle.join().unwrap();

    if results.is_abort {
        return Ok(());
    }

    if results.selected_items.is_empty() {
        return Ok(());
    }
    let item = Arc::clone(&results.selected_items[0]);
    let chosen: &Selectable = (*item).as_any().downcast_ref::<Selectable>().unwrap();

    let tmux_session = Session::new(chosen);

    if tmux_session.tmux_running() {
        if tmux_session.exists().unwrap_or(false) {
            tmux_session.switch_client().unwrap();
        } else {
            tmux_session.create_session().unwrap();
        }
    } else {
        tmux_session.create_session().unwrap();
    }

    Ok(())
}

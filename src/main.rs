fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut initial_subreddit: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--version" | "-V" => {
                println!("Reddix {}", reddix::VERSION);
                return;
            }
            "--help" | "-h" => {
                println!(
                    "Reddix — Reddit, refined for the terminal.\n\n  --version, -V        Show version and exit\n  --help,    -h        Show this help message\n  -r, --subreddit NAME Open with this subreddit selected (e.g. -r apple)\n  --check-updates      Check for updates and exit"
                );
                return;
            }
            "-r" | "--subreddit" => {
                i += 1;
                if i < args.len() && !args[i].starts_with('-') {
                    initial_subreddit = Some(args[i].clone());
                }
                i += 1;
                continue;
            }
            "--check-updates" => {
                if let Err(err) = check_updates_once() {
                    eprintln!("Update check failed: {err:?}");
                    std::process::exit(1);
                }
                return;
            }
            _ => {
                i += 1;
            }
        }
    }

    let run_opts = if initial_subreddit.is_some() {
        Some(reddix::RunOptions {
            initial_subreddit,
            ..Default::default()
        })
    } else {
        None
    };

    if let Err(err) = reddix::run(run_opts) {
        eprintln!("error: {err:?}");
        std::process::exit(1);
    }
}

fn check_updates_once() -> anyhow::Result<()> {
    use semver::Version;

    let skip_env = reddix::update::SKIP_UPDATE_ENV;
    if std::env::var(skip_env).is_ok() {
        println!("Update check skipped: {skip_env} is set.");
        return Ok(());
    }

    let current = Version::parse(reddix::VERSION)?;
    match reddix::update::check_for_update(&current)? {
        Some(info) => {
            let reddix::update::UpdateInfo {
                version,
                release_url,
                ..
            } = info;
            println!("Update available: {current} -> {version}\n{release_url}");
        }
        None => {
            println!("Reddix {current} is up to date.");
        }
    }
    Ok(())
}

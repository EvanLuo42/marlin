//! `marlin-cookd <project> [--listen <addr>] [--store <addr>] [--workers <n>]`: serves the
//! project (a directory or its `.mproject` file), the engine's content with it, to games (default port 47200 on localhost), cooking on demand into its
//! local cache, in front of a shared store if given.

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use marlin_asset::dev::PORT;
use marlin_asset_db::{Options, Project};
use marlin_cookd::{Cookd, CookdOptions, bind};
use marlin_cooker::{engine_cookers, open_ddc};

const USAGE: &str =
    "usage: marlin-cookd <project> [--listen <addr>] [--store <addr>] [--workers <n>]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((root, rest)) = args.split_first() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let mut listen = SocketAddr::from(([127, 0, 0, 1], PORT));
    let mut store = None;
    let mut options = CookdOptions::default();
    let mut rest = rest.iter();
    while let Some(flag) = rest.next() {
        let ok = match (flag.as_str(), rest.next()) {
            ("--listen", Some(a)) => a.parse().map(|a| listen = a).is_ok(),
            ("--store", Some(a)) => a.parse().map(|a| store = Some(a)).is_ok(),
            ("--workers", Some(n)) => n.parse().map(|n| options.workers = n).is_ok(),
            _ => false,
        };
        if !ok {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    }
    let opened = Project::open(root).and_then(|p| {
        let (content, _) = p.open_content(&Options::default())?;
        Ok((p, content))
    });
    let (project, content) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            eprintln!("{root}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let ddc = match open_ddc(&project.ddc_dir(), store) {
        Ok(ddc) => ddc,
        Err(e) => {
            eprintln!("opening the cache: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut server = match bind(listen) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{listen}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut cookd = Cookd::new(content, engine_cookers(), ddc, &options);
    println!(
        "marlin-cookd: {} objects in {} (engine {}), listening on {}",
        cookd.object_count(),
        project.dir.display(),
        project.engine.display(),
        server.local_addr()
    );
    let stop = AtomicBool::new(false);
    match cookd.serve(&mut server, &stop) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

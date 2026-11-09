//! openapi-ts-rs: generates TypeScript types from OpenAPI 3 specs in openapi-typescript 6.7.0's shape.
//!
//! Usage: openapi-ts-rs <spec.yml> [-o <out.d.ts>] [--strict] [--named-exports] [--ref-root <dir>]

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod emit;
mod load;
mod ts;
mod yaml;

use anyhow::{Result, bail};
use std::path::PathBuf;

const USAGE: &str = "usage: openapi-ts-rs <spec.yml> [-o <out.d.ts>] [--strict] [--named-exports] [--ref-root <dir>]\n\n  --strict          fail when a $ref does not resolve (default: warn)\n  --named-exports   also export every components.schemas entry by name\n  --ref-root <dir>  key external[...] by path relative to <dir> (default: the spec's directory)";

fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let (mut input, mut output, mut strict, mut named_exports, mut ref_root) = (None, None, false, false, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" | "--output" => output = args.next().map(PathBuf::from),
            "--strict" => strict = true,
            "--named-exports" => named_exports = true,
            "--ref-root" => ref_root = args.next().map(PathBuf::from),
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("openapi-ts-rs {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ if input.is_none() => input = Some(PathBuf::from(a)),
            _ => bail!("unexpected argument {a}"),
        }
    }
    let Some(input) = input else { bail!("{USAGE}") };
    let t = std::time::Instant::now();
    let spec = load::Spec::load(&input, ref_root.as_deref())?;
    let t1 = t.elapsed();
    let g = emit::Gen::new(&spec);
    let mut ts = g.generate();
    if named_exports {
        ts.push_str(&g.named_exports());
    }
    if std::env::var_os("OPENAPI_TS_RS_TIMING").is_some() {
        eprintln!("load {t1:?}, emit {:?}", t.elapsed() - t1);
    }
    let errors = g.errors.take();
    if !errors.is_empty() {
        let msg = format!("{} unresolved $ref(s):\n  {}", errors.len(), errors.join("\n  "));
        if strict {
            bail!(msg);
        }
        eprintln!("warning: {msg}");
    }
    match output {
        Some(p) => {
            if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir)?;
            }
            // Rewriting an unchanged file costs more than comparing it, and keeps its mtime for watchers.
            if std::fs::read(&p).ok().as_deref() != Some(ts.as_bytes()) {
                std::fs::write(p, ts)?;
            }
        }
        None => print!("{ts}"),
    }
    // Freeing every parsed document only delays the exit.
    std::mem::forget(g);
    std::mem::forget(spec);
    Ok(())
}

/// mimalloc commits memory in large regions that transparent huge pages back with 2 MiB pages,
/// tripling peak RSS for no measurable speedup.
#[cfg(target_os = "linux")]
fn disable_transparent_huge_pages() {
    const PR_SET_THP_DISABLE: i32 = 41;
    unsafe extern "C" {
        fn prctl(option: i32, ...) -> i32;
    }
    let (on, unused) = (1 as std::ffi::c_ulong, 0 as std::ffi::c_ulong);
    unsafe { prctl(PR_SET_THP_DISABLE, on, unused, unused, unused) };
}

fn main() {
    #[cfg(target_os = "linux")]
    disable_transparent_huge_pages();
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

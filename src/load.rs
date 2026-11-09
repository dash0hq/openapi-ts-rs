//! Loads a spec and every local file it references, and decides how each external file is emitted.

use anyhow::{Context, Result, bail};
use indexmap::IndexMap;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

/// How an external file is rendered under `external[...]`: openapi-typescript 6.7.0's "hint".
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// An OpenAPI document (or fragment of one) rendered as `paths`/`webhooks`/`components`/`$defs`.
    Document,
    /// A map of schemas (`file.yml#/Name` refs), rendered entry by entry.
    SchemaMap,
    Schema,
    Parameter,
    ParameterList,
    RequestBody,
    Response,
    MediaType,
    Operation,
}

pub struct File {
    pub doc: Value,
    pub kind: Kind,
    pub path: PathBuf,
}

/// Parsed files by path, or the error that reading or parsing them produced.
pub type Obj = serde_json::Map<String, Value>;

type Parsed = HashMap<PathBuf, Result<Loaded, String>>;

/// A parsed file and its first `$ref` to each other file, found while parsing in parallel.
struct Loaded {
    doc: Value,
    refs: Vec<FileRef>,
    /// The refs as found once `components.examples` is dropped, when the file has examples.
    refs_without_examples: Option<Vec<FileRef>>,
}

/// A ref's location (`None` for an array index) and the ref.
type FileRef = (Vec<Option<String>>, String);

impl Loaded {
    fn new(doc: Value) -> Loaded {
        let refs = |skip_examples| {
            let mut out = vec![];
            collect_refs(&doc, &mut vec![], &mut Default::default(), &mut out, skip_examples);
            out.into_iter()
                .map(|(at, r)| {
                    let at = at.iter().map(|s| if let Seg::K(k) = s { Some(k.to_string()) } else { None }).collect();
                    (at, r.to_string())
                })
                .collect()
        };
        let has_examples = doc.get("components").and_then(|c| c.get("examples")).is_some();
        Loaded { refs: refs(false), refs_without_examples: has_examples.then(|| refs(true)), doc }
    }
}

pub struct Spec {
    pub root: PathBuf,
    /// Directory that external file ids are relative to; the root file's directory by default.
    ref_root: PathBuf,
    /// Every file reachable from the root, parsed in parallel ahead of the ordered pass.
    parsed: Parsed,
    /// Keyed by path relative to `ref_root`; the root itself is `"."`.
    /// Ordered depth-first in reference order, like openapi-typescript's loader.
    pub files: IndexMap<String, File>,
}

pub struct Ref {
    /// Target file id (`"."` for the root).
    pub file: String,
    /// Decoded JSON pointer segments.
    pub pointer: Vec<String>,
}

pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

fn relative(from_dir: &Path, to: &Path) -> String {
    let from: Vec<_> = from_dir.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts = vec!["..".to_string(); from.len() - common];
    parts.extend(to[common..].iter().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    parts.join("/")
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(n) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(n);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Splits `file.yml#/a/b~1c` into the file part and decoded pointer segments.
pub fn split_ref(r: &str) -> (&str, Vec<String>) {
    let (file, frag) = r.split_once('#').unwrap_or((r, ""));
    let pointer = frag
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.contains(['%', '~']) { percent_decode(s).replace("~1", "/").replace("~0", "~") } else { s.to_string() }
        })
        .collect();
    (file, pointer)
}

/// A location segment; array indices matter to the hint rules.
#[derive(Clone)]
enum Seg<'a> {
    K(&'a str),
    I,
}

fn seg<'a>(p: &[Seg<'a>], i: usize) -> &'a str {
    match p.get(i) {
        Some(Seg::K(k)) => k,
        _ => "",
    }
}

/// Port of 6.7.0's `getHint`: the kind of a file referenced from location `p` (which ends with
/// the ref's file name and pointer) inside a file of kind `start`.
fn get_hint(p: &[Seg], start: Kind) -> Kind {
    let t = |n: usize| p.get(n..).unwrap_or(&[]);
    fn schema(p: &[Seg]) -> Kind {
        match seg(p, 0) {
            "allOf" | "anyOf" | "oneOf" => Kind::SchemaMap,
            _ if p.len() >= 2 => Kind::SchemaMap,
            _ => Kind::Schema,
        }
    }
    fn media(p: &[Seg]) -> Kind {
        if seg(p, 0) == "schema" { schema(&p[1..]) } else { Kind::MediaType }
    }
    fn tail<'p, 'a>(p: &'p [Seg<'a>], n: usize) -> &'p [Seg<'a>] {
        p.get(n..).unwrap_or(&[])
    }
    fn body(p: &[Seg]) -> Kind {
        if seg(p, 0) == "content" { media(tail(p, 2)) } else { Kind::RequestBody }
    }
    fn response(p: &[Seg]) -> Kind {
        match seg(p, 0) {
            "headers" => schema(tail(p, 2)),
            "content" => media(tail(p, 2)),
            _ => Kind::Response,
        }
    }
    fn operation(p: &[Seg]) -> Kind {
        match seg(p, 0) {
            "parameters" => Kind::ParameterList,
            "requestBody" => body(tail(p, 1)),
            "responses" => response(tail(p, 2)),
            _ => Kind::Operation,
        }
    }
    fn parameter(p: &[Seg]) -> Kind {
        match seg(p, 0) {
            "content" => media(tail(p, 2)),
            "schema" => schema(tail(p, 1)),
            _ => Kind::Parameter,
        }
    }
    fn path_item(p: &[Seg]) -> Kind {
        match (seg(p, 0), p.get(1)) {
            ("parameters", Some(Seg::I)) => Kind::ParameterList,
            ("parameters", _) => parameter(tail(p, 1)),
            _ => operation(tail(p, 1)),
        }
    }
    match start {
        Kind::Document => {}
        Kind::Operation => return operation(p),
        Kind::RequestBody => return body(p),
        Kind::Response => return response(p),
        Kind::SchemaMap => return Kind::Schema,
        k => return k,
    }
    match seg(p, 0) {
        "paths" => {
            let s2 = seg(p, 2).to_ascii_lowercase();
            if [".yaml", ".yml", ".json"].iter().any(|e| s2.contains(e)) { Kind::SchemaMap } else { path_item(t(2)) }
        }
        "components" => match seg(p, 1) {
            "schemas" | "headers" => schema(t(3)),
            "parameters" => parameter(t(3)),
            "responses" => response(t(3)),
            "requestBodies" => body(t(3)),
            "pathItems" => path_item(t(3)),
            _ => Kind::Schema,
        },
        _ => Kind::Document,
    }
}

impl Spec {
    pub fn load(input: &Path, ref_root: Option<&Path>) -> Result<Spec> {
        let cwd = std::env::current_dir()?;
        let root = normalize(&cwd.join(input));
        let ref_root = match ref_root {
            Some(dir) => normalize(&cwd.join(dir)),
            None => root.parent().unwrap().to_path_buf(),
        };
        let mut spec = Spec { root: root.clone(), ref_root, parsed: parse_all(&root), files: IndexMap::new() };
        spec.load_file(&root, Kind::Document)?;
        Ok(spec)
    }

    pub fn id_of(&self, path: &Path) -> String {
        if path == self.root { ".".into() } else { relative(&self.ref_root, path) }
    }

    fn load_file(&mut self, path: &Path, kind: Kind) -> Result<()> {
        let id = self.id_of(path);
        if self.files.contains_key(&id) {
            return Ok(());
        }
        let loaded = match self.parsed.remove(path) {
            Some(Ok(l)) => l,
            Some(Err(e)) => bail!(e),
            None => bail!("{} was not loaded", path.display()),
        };
        let mut doc = loaded.doc;
        let refs = match loaded.refs_without_examples {
            Some(refs) if kind == Kind::Document => {
                if let Some(Value::Object(c)) = doc.get_mut("components") {
                    c.shift_remove("examples");
                }
                refs
            }
            _ => loaded.refs,
        };
        self.files.insert(id, File { doc: Value::Null, kind, path: path.to_path_buf() });
        for (at, r) in &refs {
            let (file, pointer) = split_ref(r);
            if file.is_empty() {
                continue;
            }
            if file.contains("://") || file.starts_with("//") {
                bail!("{}: remote $ref {r} is not supported", path.display());
            }
            let target = normalize(&path.parent().unwrap().join(percent_decode(file)));
            let child = match pointer.first().map(String::as_str) {
                Some("paths" | "components") => Kind::Document,
                _ => {
                    let mut p: Vec<Seg> = at.iter().map(|s| s.as_deref().map_or(Seg::I, Seg::K)).collect();
                    p.push(Seg::K(file));
                    p.extend(pointer.iter().map(|s| Seg::K(s)));
                    get_hint(&p, kind)
                }
            };
            self.load_file(&target, child).with_context(|| format!("$ref {r} in {}", path.display()))?;
        }
        let id = self.id_of(path);
        self.files[&id].doc = doc;
        Ok(())
    }

    /// Resolves a `$ref` string found in file `from`.
    pub fn parse_ref(&self, from: &str, r: &str) -> Ref {
        let (file, pointer) = split_ref(r);
        let file = if file.is_empty() {
            from.to_string()
        } else {
            let base = &self.files[from].path;
            self.id_of(&normalize(&base.parent().unwrap().join(percent_decode(file))))
        };
        Ref { file, pointer }
    }

    pub fn resolve(&self, r: &Ref) -> Option<&Value> {
        let mut v = &self.files.get(&r.file)?.doc;
        for seg in &r.pointer {
            v = match v {
                Value::Object(o) => o.get(seg)?,
                Value::Array(a) => a.get(seg.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        Some(v)
    }
}

fn ref_target(from: &Path, r: &str) -> Option<PathBuf> {
    let (file, _) = r.split_once('#').unwrap_or((r, ""));
    if file.is_empty() || file.contains("://") || file.starts_with("//") {
        return None;
    }
    Some(normalize(&from.parent().unwrap().join(percent_decode(file))))
}

fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))
}

fn parse(path: &Path, src: &str) -> Result<Value, String> {
    let mut doc = crate::yaml::parse(src).map_err(|e| format!("parsing {}: {e:#}", path.display()))?;
    strip_x_refs(&mut doc);
    Ok(doc)
}

/// The file parts of what look like `$ref` values in YAML or JSON text. A guess, used only to
/// start parsing referenced files before their parent is parsed; the parsed refs are authoritative.
fn guess_ref_files(src: &str) -> indexmap::IndexSet<&str> {
    let mut out = indexmap::IndexSet::new();
    for (i, _) in src.match_indices("$ref") {
        let rest = src[i + 4..].trim_start_matches(['"', '\'']).trim_start_matches([' ', '\t']);
        let Some(rest) = rest.strip_prefix(':') else { continue };
        let rest = rest.trim_start_matches([' ', '\t']);
        let (rest, end): (&str, &[char]) = match rest.as_bytes().first() {
            Some(b'"') => (&rest[1..], &['"', '#', '\n']),
            Some(b'\'') => (&rest[1..], &['\'', '#', '\n']),
            _ => (rest, &['#', '\n', '\r', ' ', '\t', ',', '}', ']']),
        };
        let file = &rest[..rest.find(end).unwrap_or(rest.len())];
        if !file.is_empty() {
            out.insert(file);
        }
    }
    out
}

/// Reads and parses `root` and every file it references, transitively, in parallel.
fn parse_all(root: &Path) -> Parsed {
    fn spawn<'s>(scope: &rayon::Scope<'s>, from: &Path, files: indexmap::IndexSet<&str>, out: &'s Mutex<Parsed>) {
        let targets: Vec<PathBuf> = files.into_iter().filter_map(|f| ref_target(from, f)).collect();
        let mut seen = out.lock().unwrap();
        for t in targets {
            if !seen.contains_key(&t) {
                seen.insert(t.clone(), Err(String::new()));
                scope.spawn(move |s| visit(s, t, out));
            }
        }
    }
    fn visit<'s>(scope: &rayon::Scope<'s>, path: PathBuf, out: &'s Mutex<Parsed>) {
        let doc = read(&path).and_then(|src| {
            spawn(scope, &path, guess_ref_files(&src), out);
            parse(&path, &src)
        });
        let loaded = doc.map(Loaded::new);
        if let Ok(l) = &loaded {
            let files = l.refs.iter().map(|(_, r)| r.split_once('#').map_or(r.as_str(), |(f, _)| f)).collect();
            spawn(scope, &path, files, out);
        }
        out.lock().unwrap().insert(path, loaded);
    }
    let out = Mutex::new(HashMap::new());
    out.lock().unwrap().insert(root.to_path_buf(), Err(String::new()));
    rayon::scope(|s| visit(s, root.to_path_buf(), &out));
    out.into_inner().unwrap()
}

fn is_x_ref(r: &str) -> bool {
    r.contains("/x-") && split_ref(r).1.iter().any(|p| p.starts_with("x-"))
}

/// 6.7.0 drops refs into `x-` extensions from compositions, and deletes other such refs that
/// point into another file, leaving the rest of the object.
fn strip_x_refs(v: &mut Value) {
    match v {
        Value::Object(o) => {
            for k in ["allOf", "anyOf", "oneOf"] {
                if let Some(Value::Array(items)) = o.get_mut(k) {
                    items.retain(|i| !matches!(i.get("$ref"), Some(Value::String(r)) if is_x_ref(r)));
                }
            }
            if matches!(o.get("$ref"), Some(Value::String(r)) if is_x_ref(r) && !r.starts_with('#')) {
                o.shift_remove("$ref");
            }
            o.values_mut().for_each(strip_x_refs);
        }
        Value::Array(a) => a.iter_mut().for_each(strip_x_refs),
        _ => {}
    }
}

/// The first `$ref` to each other file, with its location. Only the first matters: it decides
/// the file's kind, and later refs to the same file find it already loaded.
fn collect_refs<'a>(
    v: &'a Value,
    path: &mut Vec<Seg<'a>>,
    seen: &mut std::collections::HashSet<&'a str>,
    out: &mut Vec<(Vec<Seg<'a>>, &'a str)>,
    skip_examples: bool,
) {
    match v {
        Value::Object(o) => {
            if let Some(r) = o.get("$ref").and_then(Value::as_str) {
                let file = r.split_once('#').map_or(r, |(f, _)| f);
                if !file.is_empty() && seen.insert(file) {
                    out.push((path.clone(), r));
                }
            }
            for (k, x) in o {
                if skip_examples && k == "examples" && matches!(path.as_slice(), [Seg::K("components")]) {
                    continue;
                }
                path.push(Seg::K(k));
                collect_refs(x, path, seen, out, skip_examples);
                path.pop();
            }
        }
        Value::Array(a) => {
            for x in a {
                path.push(Seg::I);
                collect_refs(x, path, seen, out, skip_examples);
                path.pop();
            }
        }
        _ => {}
    }
}

/// Visits every object in `v` with its path.
pub fn walk<'a>(
    v: &'a Value,
    path: &mut Vec<std::borrow::Cow<'a, str>>,
    f: &mut dyn FnMut(&'a Obj, &[std::borrow::Cow<'a, str>]),
) {
    match v {
        Value::Object(o) => {
            f(o, path);
            for (k, x) in o {
                path.push(k.as_str().into());
                walk(x, path, f);
                path.pop();
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                path.push(i.to_string().into());
                walk(x, path, f);
                path.pop();
            }
        }
        _ => {}
    }
}

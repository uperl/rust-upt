//! Table rendering for each MetaCPAN document type.
//!
//! Every function here takes the raw [`Value`] returned by the API, projects it
//! onto the crate's typed view, and prints one or more `comfy-table` tables to
//! stdout. JSON output is handled elsewhere ([`crate::json`]); this module is
//! only reached when the user wants the default human-readable form.

use std::io::IsTerminal;

use anyhow::{Context, Result};
use comfy_table::{Attribute, Cell, ContentArrangement, Table, presets::UTF8_FULL};
use metacpan_api_modern::types::{
    Author, Changes, Distribution, DownloadUrl, File, Mirror, Permission, Release,
};
use serde_json::Value;

/// Accumulator for the rows of a two-column "field / value" table. Empty and
/// missing values are dropped so the table only shows what the API actually
/// returned.
#[derive(Default)]
struct Fields(Vec<(String, String)>);

impl Fields {
    fn new() -> Self {
        Self::default()
    }

    /// Add `Some` scalar values (anything `Display`), skipping `None` and the
    /// empty string.
    fn opt<T: ToString>(&mut self, key: &str, value: &Option<T>) {
        if let Some(v) = value {
            self.text(key, v.to_string());
        }
    }

    /// Add a value that is already known.
    fn text(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        if !value.is_empty() {
            self.0.push((key.to_string(), value));
        }
    }

    /// Add a comma-joined list, skipping it entirely when empty.
    fn list<T: ToString>(&mut self, key: &str, values: &[T]) {
        if !values.is_empty() {
            let joined = values
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            self.text(key, joined);
        }
    }
}

/// A fresh table with the shared house style.
///
/// `comfy-table` reads the terminal width itself when stdout is a TTY; when the
/// output is a pipe or file there is nothing to detect, so fall back to a fixed
/// width that keeps long values wrapped rather than sprawling.
fn table() -> Table {
    let mut t = Table::new();
    t.load_preset(UTF8_FULL);
    t.set_content_arrangement(ContentArrangement::Dynamic);
    if !std::io::stdout().is_terminal() {
        t.set_width(100);
    }
    t
}

/// Build header cells, emphasised when colour is enabled.
fn header<S: ToString>(cells: impl IntoIterator<Item = S>, color: bool) -> Vec<Cell> {
    cells
        .into_iter()
        .map(|c| {
            let cell = Cell::new(c.to_string());
            if color {
                cell.add_attribute(Attribute::Bold)
            } else {
                cell
            }
        })
        .collect()
}

/// Print a "field / value" table for a single document.
fn print_fields(fields: Fields, color: bool) {
    let mut t = table();
    t.set_header(header(["Field", "Value"], color));
    for (k, v) in fields.0 {
        t.add_row(vec![Cell::new(k), Cell::new(v)]);
    }
    println!("{t}");
}

// ---------------------------------------------------------------------------

pub fn author(value: Value, color: bool) -> Result<()> {
    let a: Author = serde_json::from_value(value).context("decoding author response")?;
    let mut f = Fields::new();
    f.opt("pauseid", &a.pauseid);
    f.opt("name", &a.name);
    f.opt("asciiname", &a.asciiname);
    f.list("email", &a.email);
    f.list("website", &a.website);
    f.opt("city", &a.city);
    f.opt("region", &a.region);
    f.opt("country", &a.country);
    if !a.profile.is_empty() {
        let profiles = a
            .profile
            .iter()
            .filter_map(|p| match (&p.name, &p.id) {
                (Some(n), Some(i)) => Some(format!("{n}:{i}")),
                (Some(n), None) => Some(n.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        f.list("profile", &profiles);
    }
    if let Some(rc) = &a.release_count {
        f.text(
            "releases",
            format!(
                "cpan {}, latest {}, backpan-only {}",
                rc.cpan.unwrap_or(0),
                rc.latest.unwrap_or(0),
                rc.backpan_only.unwrap_or(0),
            ),
        );
    }
    f.opt("updated", &a.updated);
    print_fields(f, color);
    Ok(())
}

pub fn release(value: Value, color: bool) -> Result<()> {
    let r: Release = serde_json::from_value(value).context("decoding release response")?;
    let mut f = Fields::new();
    f.opt("name", &r.name);
    f.opt("distribution", &r.distribution);
    f.opt("author", &r.author);
    f.opt("version", &r.version);
    f.opt("abstract", &r.r#abstract);
    f.opt("date", &r.date);
    f.opt("status", &r.status);
    f.opt("maturity", &r.maturity);
    f.opt("authorized", &r.authorized);
    f.opt("deprecated", &r.deprecated);
    f.list("license", &r.license);
    f.opt("main_module", &r.main_module);
    f.opt("archive", &r.archive);
    f.opt("download_url", &r.download_url);
    f.opt("checksum_sha256", &r.checksum_sha256);
    if !r.dependency.is_empty() {
        f.text("dependencies", r.dependency.len().to_string());
    }
    print_fields(f, color);

    if !r.dependency.is_empty() {
        let mut t = table();
        t.set_header(header(
            ["phase", "relationship", "module", "version"],
            color,
        ));
        for d in &r.dependency {
            t.add_row(vec![
                Cell::new(d.phase.as_deref().unwrap_or("-")),
                Cell::new(d.relationship.as_deref().unwrap_or("-")),
                Cell::new(d.module.as_deref().unwrap_or("-")),
                Cell::new(d.version.as_deref().unwrap_or("0")),
            ]);
        }
        println!("{t}");
    }
    Ok(())
}

pub fn file_doc(value: Value, color: bool) -> Result<()> {
    let file: File = serde_json::from_value(value).context("decoding file response")?;
    let mut f = Fields::new();
    f.opt("name", &file.name);
    f.opt("path", &file.path);
    f.opt("distribution", &file.distribution);
    f.opt("author", &file.author);
    f.opt("release", &file.release);
    f.opt("version", &file.version);
    f.opt("abstract", &file.r#abstract);
    f.opt("documentation", &file.documentation);
    f.opt("mime", &file.mime);
    f.opt("status", &file.status);
    f.opt("maturity", &file.maturity);
    f.opt("date", &file.date);
    f.opt("authorized", &file.authorized);
    f.opt("indexed", &file.indexed);
    f.opt("deprecated", &file.deprecated);
    f.opt("sloc", &file.sloc);
    f.opt("slop", &file.slop);
    f.opt("download_url", &file.download_url);
    let modules = file
        .module
        .iter()
        .filter_map(|m| m.name.clone())
        .collect::<Vec<_>>();
    f.list("modules", &modules);
    print_fields(f, color);
    Ok(())
}

pub fn distribution(value: Value, color: bool) -> Result<()> {
    let d: Distribution =
        serde_json::from_value(value).context("decoding distribution response")?;
    let mut f = Fields::new();
    f.opt("name", &d.name);
    if let Some(r) = &d.river {
        f.opt("river bucket", &r.bucket);
        f.opt("river bus_factor", &r.bus_factor);
        f.opt("river immediate", &r.immediate);
        f.opt("river total", &r.total);
    }
    if !d.external_package.is_empty() {
        let pkgs = d
            .external_package
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>();
        f.list("external packages", &pkgs);
    }
    if let Some(bugs) = &d.bugs {
        summarise_counts(&mut f, "bugs", bugs);
    }
    print_fields(f, color);
    Ok(())
}

/// MetaCPAN's `bugs` / `repo` blocks are `{ tracker: { active: N, closed: N,
/// ... } }`. Flatten one level of numeric leaves into `bugs (tracker)` rows.
fn summarise_counts(f: &mut Fields, prefix: &str, value: &Value) {
    let Some(obj) = value.as_object() else { return };
    for (tracker, body) in obj {
        let Some(inner) = body.as_object() else {
            continue;
        };
        let parts = inner
            .iter()
            .filter_map(|(k, v)| v.as_u64().map(|n| format!("{k} {n}")))
            .collect::<Vec<_>>();
        if !parts.is_empty() {
            f.text(&format!("{prefix} ({tracker})"), parts.join(", "));
        }
    }
}

pub fn changes(value: Value, color: bool) -> Result<()> {
    let c: Changes = serde_json::from_value(value).context("decoding changes response")?;
    let mut f = Fields::new();
    f.opt("name", &c.name);
    f.opt("distribution", &c.distribution);
    f.opt("author", &c.author);
    f.opt("release", &c.release);
    f.opt("category", &c.category);
    print_fields(f, color);
    if let Some(content) = &c.content
        && !content.is_empty()
    {
        println!("{content}");
    }
    Ok(())
}

pub fn download_url(value: Value, color: bool) -> Result<()> {
    let d: DownloadUrl = serde_json::from_value(value).context("decoding download_url response")?;
    let mut f = Fields::new();
    f.opt("download_url", &d.download_url);
    f.opt("version", &d.version);
    f.opt("release", &d.release);
    f.opt("distribution", &d.distribution);
    f.opt("status", &d.status);
    f.opt("date", &d.date);
    f.opt("checksum_sha256", &d.checksum_sha256);
    print_fields(f, color);
    Ok(())
}

pub fn mirrors(value: Value, color: bool) -> Result<()> {
    let list: Vec<Mirror> = serde_json::from_value(value).context("decoding mirror response")?;
    let mut t = table();
    t.set_header(header(["name", "org", "country", "freq", "url"], color));
    for m in &list {
        let url = m
            .http
            .as_deref()
            .or(m.ftp.as_deref())
            .or(m.rsync.as_deref())
            .unwrap_or("-");
        t.add_row(vec![
            Cell::new(m.name.as_deref().unwrap_or("-")),
            Cell::new(m.org.as_deref().unwrap_or("-")),
            Cell::new(m.country.as_deref().unwrap_or("-")),
            Cell::new(m.freq.as_deref().unwrap_or("-")),
            Cell::new(url),
        ]);
    }
    println!("{t}");
    println!("{} mirrors", list.len());
    Ok(())
}

/// One row of `river` / `adoptable` output: a distribution, an optional label
/// column shown right after it (the latest-release author for `river
/// distribution`, the `ADOPTME`/`HANDOFF` status for `adoptable`), and its
/// CPAN River figures. Every optional field is `None` when there is no value.
pub struct RiverRow {
    pub distribution: String,
    pub label: Option<String>,
    pub total: Option<u64>,
    pub immediate: Option<u64>,
    pub bucket: Option<u64>,
}

/// Print the `river` table, in the order given. `label_header`, when set, adds
/// the label column right after `distribution` under that heading; `river
/// author` passes `None` since every row is the same queried author. The
/// caller prints its own summary line.
pub fn river(rows: &[RiverRow], label_header: Option<&str>, color: bool) {
    let mut cols = vec!["distribution"];
    cols.extend(label_header);
    cols.extend(["river total", "river immediate", "bucket"]);

    let mut t = table();
    t.set_header(header(cols, color));
    for r in rows {
        let mut cells = vec![Cell::new(&r.distribution)];
        if label_header.is_some() {
            cells.push(Cell::new(r.label.as_deref().unwrap_or("-")));
        }
        cells.push(Cell::new(opt_num(r.total)));
        cells.push(Cell::new(opt_num(r.immediate)));
        cells.push(Cell::new(opt_num(r.bucket)));
        t.add_row(cells);
    }
    println!("{t}");
}

/// A count for a table cell, or `-` when the API had no number for it.
fn opt_num(n: Option<u64>) -> String {
    n.map_or_else(|| "-".to_string(), |n| n.to_string())
}

/// Print a `module / owner / co-maintainers` table for `permissions`.
pub fn permissions(perms: &[Permission], color: bool) {
    let mut t = table();
    t.set_header(header(["module", "owner", "co-maintainers"], color));
    for p in perms {
        let co = if p.co_maintainers.is_empty() {
            "-".to_string()
        } else {
            p.co_maintainers.join(", ")
        };
        t.add_row(vec![
            Cell::new(p.module_name.as_deref().unwrap_or("-")),
            Cell::new(p.owner.as_deref().unwrap_or("-")),
            Cell::new(co),
        ]);
    }
    println!("{t}");
    if perms.len() != 1 {
        println!("{} modules", perms.len());
    }
}

pub fn search(value: Value, type_: &str, color: bool) -> Result<()> {
    let total = value
        .pointer("/hits/total/value")
        .or_else(|| value.pointer("/hits/total"))
        .and_then(Value::as_u64);
    let empty = Vec::new();
    let hits = value
        .pointer("/hits/hits")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    let columns: &[&str] = match type_ {
        "release" => &["name", "author", "date", "status", "abstract"],
        "author" => &["pauseid", "name", "country"],
        "module" | "file" => &["name", "distribution", "author", "version"],
        "distribution" => &["name", "river.total"],
        "favorite" => &["distribution", "user", "date"],
        _ => &["_id", "_score"],
    };

    let mut t = table();
    t.set_header(header(columns.iter().copied(), color));
    for hit in hits {
        let source = hit.get("_source").unwrap_or(&Value::Null);
        let row: Vec<Cell> = columns
            .iter()
            .map(|col| {
                let raw = match *col {
                    "_id" => hit.get("_id"),
                    "_score" => hit.get("_score"),
                    path => source.pointer(&format!("/{}", path.replace('.', "/"))),
                };
                Cell::new(scalar(raw))
            })
            .collect();
        t.add_row(row);
    }
    println!("{t}");
    if let Some(total) = total {
        println!("{} matching (showing {})", total, hits.len());
    }
    Ok(())
}

/// Render a JSON scalar for a table cell; anything non-scalar (or missing)
/// becomes `-`.
fn scalar(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => "-".to_string(),
    }
}

use owo_colors::OwoColorize;
use serde_json;
use std::{
    cmp::Ordering::{self},
    fmt::Display,
    fs::File,
    io::{BufReader, ErrorKind},
    ops::Deref,
};
use versions::Versioning;

use camino::{Utf8Path, Utf8PathBuf};
use miette::IntoDiagnostic;
use rustc_hash::FxHashMap;

#[derive(Debug, Copy, Clone, Default, serde::Deserialize, clap::ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum DiffStyle {
    /// Display a diff of the whole directory tree, showing all paths which were added, removed, or
    /// changed.
    Tree,
    #[default]
    /// Display a more compact diff based on npingler manifests. Shows one entry for each derivation
    /// in your profile's `paths` which has been added, removed, or changed. Also shows changes in
    /// versions (where available).
    Manifest,
}

#[derive(Debug)]
pub enum DiffResult {
    /// This constructor means one of two things:
    ///
    /// 1) the profile paths are identical,
    /// 2) the profile paths are different but no installed paths actually changed their contents
    NoChanges,
    /// There are changes. The String field, if present, contains a formatted message (which may
    /// contain ANSI escape codes) suitable for printing to the console. None indicates that there
    /// are changes but we don't have anything to say/display about them - for example, the Tree
    /// diff style prints nothing when there is no previous profile to compare with.
    Changes(Option<String>),
}

#[derive(Debug, Clone, serde::Deserialize)]
#[allow(dead_code)]
struct Version(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Deserialize)]
struct PackageName(String);

#[derive(Debug, PartialEq, Eq, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManifestEntry {
    version: String,
    /// nixpkgs is permitted to not have a store path, but everything else should have one
    store_path: Option<Utf8PathBuf>,
}

#[derive(Debug, serde::Deserialize, Default)]
struct Manifest(FxHashMap<PackageName, ManifestEntry>);

pub fn diff(
    style: DiffStyle,
    old: Option<&Utf8Path>,
    new: &Utf8Path,
) -> miette::Result<DiffResult> {
    match style {
        DiffStyle::Tree => {
            let old = match old {
                None => return Ok(DiffResult::Changes(None)),
                Some(old) => old,
            };
            let diff =
                diff_trees::Diff::new(old.as_std_path(), new.as_std_path()).into_diagnostic()?;
            // We have to display the diff to see if it's actually empty. See https://github.com/9999years/npingler/issues/68
            let displayed_diff = format!(
                "{}",
                diff.display(diff_trees::DisplayDiffOpts::new().color(true))
            );

            if displayed_diff.is_empty() {
                Ok(DiffResult::NoChanges)
            } else {
                Ok(DiffResult::Changes(Some(displayed_diff)))
            }
        }
        DiffStyle::Manifest => {
            let old_manifest = match old {
                Some(o) => decode_manifest(o)?,
                None => None,
            }
            .unwrap_or_default();
            let new_manifest = match decode_manifest(new)? {
                Some(m) => m,
                None => {
                    // We want to handle this situation gracefully, since it can happen if the
                    // installed npingler program is newer than the npingler nix library which was
                    // used to create the profile (e.g. if someone's nixpkgs entry in their npins
                    // sources is newer than their npingler entry).
                    tracing::debug!(
                        "New profile {new} has no manifest; falling back to tree diff style"
                    );
                    return diff(DiffStyle::Tree, old, new);
                }
            };
            let diff = diff_manifests(&old_manifest, &new_manifest);
            if diff.is_empty() {
                Ok(DiffResult::NoChanges)
            } else {
                Ok(DiffResult::Changes(Some(diff.to_string())))
            }
        }
    }
}

const MANIFEST_PATH: &str = "share/npingler/manifest.json";

fn decode_manifest(path: &Utf8Path) -> miette::Result<Option<Manifest>> {
    let manifest_file = match File::open(path.join(Utf8Path::new(MANIFEST_PATH))) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).into_diagnostic(),
    };
    let reader = BufReader::new(manifest_file);

    let manifest = serde_json::from_reader(reader).into_diagnostic()?;

    Ok(Some(manifest))
}

#[derive(Debug)]
enum ManifestDiffEntry {
    Added(ManifestEntry),
    Changed(ManifestEntry, ManifestEntry),
    Removed(ManifestEntry),
}

#[derive(Debug)]
struct ManifestDiff(FxHashMap<PackageName, ManifestDiffEntry>);

impl Deref for ManifestDiff {
    type Target = FxHashMap<PackageName, ManifestDiffEntry>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

fn fmt_manifest_diff_entry(package_name: &PackageName, entry: &ManifestDiffEntry) -> String {
    let pkg = &package_name.0;
    match entry {
        ManifestDiffEntry::Added(entry) => {
            format!("+ {} ({})", pkg, entry.version).green().to_string()
        }
        ManifestDiffEntry::Changed(old_entry, new_entry) => {
            match (
                Versioning::new(&old_entry.version),
                Versioning::new(&new_entry.version),
            ) {
                (Some(old_version), Some(new_version)) => match old_version.cmp(&new_version) {
                    Ordering::Less => {
                        format!("^ {} ({} → {})", pkg, old_entry.version, new_entry.version)
                            .green()
                            .to_string()
                    }
                    Ordering::Greater => {
                        format!("v {} ({} → {})", pkg, old_entry.version, new_entry.version)
                            .red()
                            .to_string()
                    }
                    Ordering::Equal => format!("~ {} ({})", pkg, old_entry.version)
                        .yellow()
                        .to_string(),
                },
                _ => format!("~ {} ({} → {})", pkg, old_entry.version, new_entry.version)
                    .yellow()
                    .to_string(),
            }
        }
        ManifestDiffEntry::Removed(entry) => {
            format!("- {} ({})", pkg, entry.version).red().to_string()
        }
    }
}

impl Display for ManifestDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Print nixpkgs first if present
        let nixpkgs = PackageName("nixpkgs".to_string());
        if let Some(value) = self.get(&nixpkgs) {
            writeln!(f, "{}", fmt_manifest_diff_entry(&nixpkgs, value))?;
        }
        for (key, value) in self.0.iter() {
            if key == &nixpkgs {
                continue;
            }
            writeln!(f, "{}", fmt_manifest_diff_entry(key, value))?;
        }
        Ok(())
    }
}

fn diff_manifests(old: &Manifest, new: &Manifest) -> ManifestDiff {
    let mut result: FxHashMap<PackageName, ManifestDiffEntry> = Default::default();
    let old = &old.0;
    let mut new = new.0.clone();
    for (key, value) in old.iter() {
        match new.remove(key) {
            None => {
                result.insert(key.clone(), ManifestDiffEntry::Removed(value.clone()));
            }
            Some(new_value) => {
                if value != &new_value {
                    result.insert(
                        key.clone(),
                        ManifestDiffEntry::Changed(value.clone(), new_value.clone()),
                    );
                };
            }
        }
    }
    for (key, value) in new.iter() {
        result.insert(key.clone(), ManifestDiffEntry::Added(value.clone()));
    }
    tracing::debug!(?result, "diff result");
    ManifestDiff(result)
}

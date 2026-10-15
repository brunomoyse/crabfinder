//! Open a BeSt zip and pass its inner XML to a callback.
//!
//! Each BeSt zip contains exactly one `.xml` file. We don't extract to disk —
//! the consumer receives a `BufRead` over the inflated stream and the
//! decompression is driven by their reads.
//!
//! The callback API avoids the lifetime gymnastics needed to return a
//! `ZipFile<'_>` reader — the archive lives on the stack inside `with_inner_xml`
//! for as long as the callback runs.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use anyhow::{Context, Result, anyhow};

/// Open `zip_path`, locate its single `.xml` entry, and invoke `f` with a
/// buffered reader over the decompressed XML. `f`'s output is returned.
pub fn with_inner_xml<F, R>(zip_path: &Path, f: F) -> Result<R>
where
    F: FnOnce(&mut BufReader<zip::read::ZipFile<'_, File>>) -> Result<R>,
{
    let file = File::open(zip_path).with_context(|| format!("open zip {zip_path:?}"))?;
    let mut archive =
        zip::ZipArchive::new(file).with_context(|| format!("read zip header {zip_path:?}"))?;

    // Find the .xml entry's index without holding a borrow on the archive.
    let mut xml_name = None;
    for i in 0..archive.len() {
        let Some(name) = archive.name_for_index(i) else {
            continue;
        };
        let name = name.with_context(|| format!("read entry name #{i} inside {zip_path:?}"))?;
        if name.to_ascii_lowercase().ends_with(".xml") {
            xml_name = Some(name.into_owned());
            break;
        }
    }
    let xml_name = xml_name.ok_or_else(|| anyhow!("no .xml entry inside {zip_path:?}"))?;

    let entry = archive
        .by_name(&xml_name)
        .with_context(|| format!("open entry {xml_name:?} inside {zip_path:?}"))?;
    let mut reader = BufReader::with_capacity(1 << 20, entry);
    f(&mut reader)
}
